use im::vector::Vector;

use crypto::identity::PublicKey;
use crypto::uid::Uid;

use super::token_channel::directional::{DirectionalMutation, MoveTokenDirection};
use super::types::{FriendTcOp, FriendStatus, 
    RequestsStatus, RequestSendFunds, FriendMoveToken,
    ResponseSendFunds, FailureSendFunds, UserRequestSendFunds,
    ChannelToken, ResetTerms};
use super::token_channel::directional::DirectionalTc;



#[derive(Clone, Serialize, Deserialize)]
pub enum ResponseOp {
    Response(ResponseSendFunds),
    Failure(FailureSendFunds),
}

#[allow(unused)]
pub enum FriendMutation<A> {
    DirectionalMutation(DirectionalMutation),
    SetChannelStatus((ResetTerms, Option<ResetTerms>)), // (local_reset_terms, opt_remote_reset_terms)
    SetWantedRemoteMaxDebt(u128),
    SetWantedLocalRequestsStatus(RequestsStatus),
    PushBackPendingRequest(RequestSendFunds),
    PopFrontPendingRequest,
    PushBackPendingResponse(ResponseOp),
    PopFrontPendingResponse,
    PushBackPendingUserRequest(RequestSendFunds),
    PopFrontPendingUserRequest,
    SetStatus(FriendStatus),
    SetFriendAddr(A),
    LocalReset(FriendMoveToken),
    // The outgoing move token message we have sent to reset the channel.
    RemoteReset,
}

#[derive(Clone, Serialize, Deserialize)]
pub enum ChannelStatus {
    Inconsistent((ResetTerms, Option<ResetTerms>)), // local_reset_terms, remote_reset_terms
    Consistent(DirectionalTc),
}

#[allow(unused)]
#[derive(Clone, Serialize, Deserialize)]
pub struct FriendState<A> {
    pub local_public_key: PublicKey,
    pub remote_public_key: PublicKey,
    pub remote_address: A, 
    pub channel_status: ChannelStatus,
    pub wanted_remote_max_debt: u128,
    pub wanted_local_requests_status: RequestsStatus,
    pub pending_responses: Vector<ResponseOp>,
    pub pending_requests: Vector<RequestSendFunds>,
    // Pending operations to be sent to the token channel.
    pub status: FriendStatus,
    pub pending_user_requests: Vector<RequestSendFunds>,
    // Request that the user has sent to this neighbor, 
    // but have not been processed yet. Bounded in size.
}


#[allow(unused)]
impl<A:Clone> FriendState<A> {
    pub fn new(local_public_key: &PublicKey,
               remote_public_key: &PublicKey,
               remote_address: A) -> FriendState<A> {
        FriendState {
            local_public_key: local_public_key.clone(),
            remote_public_key: remote_public_key.clone(),
            remote_address,
            channel_status: ChannelStatus::Consistent(DirectionalTc::new(local_public_key,
                                           remote_public_key)),

            // The remote_max_debt we want to have. When possible, this will be sent to the remote
            // side.
            wanted_remote_max_debt: 0,
            wanted_local_requests_status: RequestsStatus::Closed,
            // The local_send_price we want to have (Or possibly close requests, by having an empty
            // send price). When possible, this will be updated with the TokenChannel.
            pending_requests: Vector::new(),
            pending_responses: Vector::new(),
            status: FriendStatus::Enable,
            pending_user_requests: Vector::new(),
        }
    }

    /// Return how much (in credits) we trust this friend.
    pub fn get_trust(&self) -> u128 {
        match &self.channel_status {
            ChannelStatus::Consistent(directional) =>
                directional.token_channel.state().balance.remote_max_debt,
            ChannelStatus::Inconsistent(_) => {
                // TODO; Is this the right return value here?
                self.wanted_remote_max_debt 
            },
        }

    }

    #[allow(unused)]
    pub fn mutate(&mut self, friend_mutation: &FriendMutation<A>) {
        match friend_mutation {
            FriendMutation::DirectionalMutation(directional_mutation) => {
                match &mut self.channel_status {
                    ChannelStatus::Consistent(ref mut directional) =>
                        directional.mutate(directional_mutation),
                    ChannelStatus::Inconsistent(_) => unreachable!(),
                }
            },
            FriendMutation::SetChannelStatus(channel_status) => {
                self.channel_status = ChannelStatus::Inconsistent(channel_status.clone());
            },
            FriendMutation::SetWantedRemoteMaxDebt(wanted_remote_max_debt) => {
                self.wanted_remote_max_debt = *wanted_remote_max_debt;
            },
            FriendMutation::SetWantedLocalRequestsStatus(wanted_local_requests_status) => {
                self.wanted_local_requests_status = wanted_local_requests_status.clone();
            },
            FriendMutation::PushBackPendingRequest(request_send_funds) => {
                self.pending_requests.push_back(request_send_funds.clone());
            },
            FriendMutation::PopFrontPendingRequest => {
                let _ = self.pending_requests.pop_front();
            },
            FriendMutation::PushBackPendingResponse(response_op) => {
                self.pending_responses.push_back(response_op.clone());
            },
            FriendMutation::PopFrontPendingResponse => {
                let _ = self.pending_responses.pop_front();
            },
            FriendMutation::PushBackPendingUserRequest(request_send_funds) => {
                self.pending_user_requests.push_back(request_send_funds.clone());
            },
            FriendMutation::PopFrontPendingUserRequest => {
                let _ = self.pending_user_requests.pop_front();
            },
            FriendMutation::SetStatus(friend_status) => {
                self.status = friend_status.clone();
            },
            FriendMutation::SetFriendAddr(friend_addr) => {
                self.remote_address = friend_addr.clone();
            },
            FriendMutation::LocalReset(reset_move_token) => {
                // Local reset was applied (We sent a reset from the control line)
                match &self.channel_status {
                    ChannelStatus::Consistent(_) => unreachable!(),
                    ChannelStatus::Inconsistent((local_reset_terms, None)) => unreachable!(),
                    ChannelStatus::Inconsistent((local_reset_terms, Some(remote_reset_terms))) => {
                        assert_eq!(reset_move_token.old_token, remote_reset_terms.reset_token);
                        let directional = DirectionalTc::new_from_local_reset(
                            &self.local_public_key,
                            &self.remote_public_key,
                            &reset_move_token,
                            remote_reset_terms.balance_for_reset);
                        self.channel_status = ChannelStatus::Consistent(directional);
                    }
                }
            },
            FriendMutation::RemoteReset => {
                // Remote reset was applied (Remote side has given a reset command)
                match &self.channel_status {
                    ChannelStatus::Consistent(_) => unreachable!(),
                    ChannelStatus::Inconsistent((local_reset_terms, _)) => {
                        let directional = DirectionalTc::new_from_remote_reset(
                            &self.local_public_key,
                            &self.remote_public_key,
                            &local_reset_terms.reset_token,
                            local_reset_terms.balance_for_reset);
                        self.channel_status = ChannelStatus::Consistent(directional);
                    },
                }
            },
        }
    }
}