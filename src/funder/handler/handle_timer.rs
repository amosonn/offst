use ring::rand::SecureRandom;
use futures::prelude::{async, await};

use crypto::identity::PublicKey;

use super::{FriendInconsistencyError, FunderTask, FriendMessage};
use super::super::liveness::Actions;
use super::super::friend::{IncomingInconsistency, 
    OutgoingInconsistency, FriendMutation, ResponseOp};
use super::super::state::FunderMutation;
use super::super::messages::{ResponseSendFundsResult};
use super::super::token_channel::directional::MoveTokenDirection;

use super::{MutableFunderHandler, ResponseReceived};


enum HandleTimerError {
}

impl<A:Clone + 'static, R:SecureRandom + 'static> MutableFunderHandler<A,R> {

    /// Create a (signed) failure message for a given request_id.
    /// We are the reporting_public_key for this failure message.
    fn invoke_actions(&mut self, 
                      remote_public_key: &PublicKey,
                      actions: &Actions) {

        if actions.retransmit_inconsistency {
            let friend = self.get_friend(&remote_public_key).unwrap();
            // Check if we have an inconsistency message to ack:
            let opt_ack = match &friend.inconsistency_status.incoming {
                IncomingInconsistency::Empty => None,
                IncomingInconsistency::Incoming(reset_terms) => Some(reset_terms.current_token.clone()),
            };

            let reset_terms = match &friend.inconsistency_status.outgoing {
                OutgoingInconsistency::Empty | OutgoingInconsistency::Acked => unreachable!(),
                OutgoingInconsistency::Sent(reset_terms) => reset_terms
            };

            let inconsistency_error = FriendInconsistencyError {
                opt_ack,
                current_token: reset_terms.current_token.clone(),
                balance_for_reset: reset_terms.balance_for_reset,
            };

            self.add_task(
                FunderTask::FriendMessage(
                    FriendMessage::InconsistencyError(inconsistency_error)));
        }

        if actions.retransmit_token_msg {
            let friend = self.get_friend(&remote_public_key).unwrap();
            let outgoing_move_token = friend.directional.get_outgoing_move_token().unwrap();
            // Add a task for sending the outgoing move token:
            self.add_task(
                FunderTask::FriendMessage(
                    FriendMessage::MoveToken(outgoing_move_token)));
        }

        if actions.retransmit_request_token {
            let friend = self.get_friend(&remote_public_key).unwrap();
            let new_token = match &friend.directional.direction {
                MoveTokenDirection::Incoming(new_token) => new_token.clone(),
                MoveTokenDirection::Outgoing(_) => unreachable!(),
            };
            self.add_task(
                FunderTask::FriendMessage(
                    FriendMessage::RequestToken(new_token)));
        }

        if actions.send_keepalive {
            self.add_task(
                FunderTask::FriendMessage(
                    FriendMessage::KeepAlive));
        }
    }

    #[async]
    fn cancel_pending_requests(mut self,
                               friend_public_key: PublicKey)
                        -> Result<Self, HandleTimerError> {

        let friend = self.get_friend(&friend_public_key).unwrap();
        let mut pending_requests = friend.pending_requests.clone();
        let mut fself = self;

        while let Some(pending_request) = pending_requests.pop_front() {
            let friend_mutation = FriendMutation::PopFrontPendingRequest;
            let messenger_mutation = FunderMutation::FriendMutation((friend_public_key.clone(), friend_mutation));
            fself.apply_mutation(messenger_mutation);

            let opt_origin_public_key = fself.find_request_origin(&pending_request.request_id).cloned();
            let origin_public_key = match opt_origin_public_key {
                Some(origin_public_key) => {
                    let pending_request = pending_request.create_pending_request();
                    let (new_fself, failure_send_funds) = await!(fself.create_failure_message(pending_request)).unwrap();
                    fself = new_fself;

                    let failure_op = ResponseOp::Failure(failure_send_funds);
                    let friend_mutation = FriendMutation::PushBackPendingResponse(failure_op);
                    let messenger_mutation = FunderMutation::FriendMutation((origin_public_key.clone(), friend_mutation));
                    fself.apply_mutation(messenger_mutation);
                },
                None => {
                    // We are the origin of this request:
                    let response_received = ResponseReceived {
                        request_id: pending_request.request_id,
                        result: ResponseSendFundsResult::Failure(fself.state.local_public_key.clone()),
                    };
                    fself.funder_tasks.push(FunderTask::ResponseReceived(response_received));
                }, 
            };
        }
        Ok(fself)
    }

    #[async]
    fn cancel_pending_user_requests(mut self,
                               friend_public_key: PublicKey)
                        -> Result<Self, HandleTimerError> {

        let friend = self.get_friend(&friend_public_key).unwrap();
        let mut pending_user_requests = friend.pending_user_requests.clone();
        let mut fself = self;

        while let Some(pending_user_request) = pending_user_requests.pop_front() {
            let friend_mutation = FriendMutation::PopFrontPendingUserRequest;
            let messenger_mutation = FunderMutation::FriendMutation((friend_public_key.clone(), friend_mutation));
            fself.apply_mutation(messenger_mutation);

            let opt_origin_public_key = fself.find_request_origin(&pending_user_request.request_id).cloned();
            let origin_public_key = match opt_origin_public_key {
                Some(origin_public_key) => {
                    let pending_request = pending_user_request.create_pending_request();
                    let (new_fself, failure_send_funds) = await!(fself.create_failure_message(pending_request)).unwrap();
                    fself = new_fself;

                    let failure_op = ResponseOp::Failure(failure_send_funds);
                    let friend_mutation = FriendMutation::PushBackPendingResponse(failure_op);
                    let messenger_mutation = FunderMutation::FriendMutation((origin_public_key.clone(), friend_mutation));
                    fself.apply_mutation(messenger_mutation);
                },
                None => {
                    // We are the origin of this request:
                    let response_received = ResponseReceived {
                        request_id: pending_user_request.request_id,
                        result: ResponseSendFundsResult::Failure(fself.state.local_public_key.clone()),
                    };
                    fself.funder_tasks.push(FunderTask::ResponseReceived(response_received));
                }, 
            };
        }
        Ok(fself)
    }

    #[async]
    fn handle_timer_tick(mut self)
                        -> Result<Self, HandleTimerError> {
        let time_tick_output = self.ephemeral.liveness.time_tick();
        for (friend_public_key, actions) in &time_tick_output.friends_actions {
            self.invoke_actions(friend_public_key, actions);
        }

        // For any friend that just got offline: Cancel all pending requests.
        let mut fself = self;
        for friend_public_key in time_tick_output.became_offline {
            fself = await!(fself.cancel_pending_requests(friend_public_key.clone()))?;
            fself = await!(fself.cancel_pending_user_requests(friend_public_key.clone()))?;
        }
        Ok(fself)
    }
}