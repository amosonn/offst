#![warn(unused)]

use std::convert::TryFrom;

use crypto::identity::{PublicKey, Signature, PUBLIC_KEY_LEN, SIGNATURE_LEN};
use crypto::crypto_rand::{RandValue, RAND_VALUE_LEN};
use crypto::hash::sha_512_256;
use identity::IdentityClient;

use crate::consts::MAX_OPERATIONS_IN_BATCH;

use crate::mutual_credit::types::{MutualCredit, McMutation};
use crate::mutual_credit::incoming::{ProcessOperationOutput, ProcessTransListError, 
    process_operations_list, IncomingMessage};
use crate::mutual_credit::outgoing::OutgoingMc;

use crate::types::{FriendMoveToken, 
    FriendMoveTokenRequest, FriendTcOp};


pub enum SetDirection {
    Incoming(FriendMoveToken), 
    Outgoing(FriendMoveToken),
}

#[allow(unused)]
pub enum TcMutation {
    McMutation(McMutation),
    SetDirection(SetDirection),
    SetTokenWanted,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct TcOutgoing {
    pub mutual_credit: MutualCredit,
    pub move_token_out: FriendMoveToken,
    pub token_wanted: bool,
    pub opt_prev_move_token_in: Option<FriendMoveToken>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct TcIncoming {
    pub mutual_credit: MutualCredit,
    pub move_token_in: FriendMoveToken,
}

#[derive(Clone, Serialize, Deserialize)]
pub enum TcDirection {
    Incoming(TcIncoming),
    Outgoing(TcOutgoing),
}


#[derive(Clone, Serialize, Deserialize)]
pub struct TokenChannel {
    direction: TcDirection,
}

#[derive(Debug)]
pub enum ReceiveMoveTokenError {
    ChainInconsistency,
    InvalidTransaction(ProcessTransListError),
    InvalidSignature,
    InvalidStatedBalance,
    InvalidInconsistencyCounter,
    MoveTokenCounterOverflow,
    InvalidMoveTokenCounter,
}

pub struct MoveTokenReceived {
    pub incoming_messages: Vec<IncomingMessage>,
    pub mutations: Vec<TcMutation>,
}


pub enum ReceiveMoveTokenOutput {
    Duplicate,
    RetransmitOutgoing(FriendMoveToken),
    Received(MoveTokenReceived),
    // In case of a reset, all the local pending requests will be canceled.
}




/// Create a token from a public key
/// Currently this function puts the public key in the beginning of the signature buffer,
/// as the public key is shorter than a signature.
/// Possibly change this in the future (Maybe use a hash to spread the public key over all the
/// bytes of the signature)
///
/// Note that the output here is not a real signature. This function is used for the first
/// deterministic initialization of a token channel.
fn token_from_public_key(public_key: &PublicKey) -> Signature {
    let mut buff = [0; SIGNATURE_LEN];
    buff[0 .. PUBLIC_KEY_LEN].copy_from_slice(public_key);
    Signature::from(buff)
}

/// Generate a random nonce from public key.
/// Note that the result here is not really a random nonce. This function is used for the first
/// deterministic initialization of a token channel.
fn rand_nonce_from_public_key(public_key: &PublicKey) -> RandValue {
    let public_key_hash = sha_512_256(public_key);
    RandValue::try_from(&public_key_hash.as_ref()[.. RAND_VALUE_LEN]).unwrap()
}

impl TokenChannel {
    pub fn new(local_public_key: &PublicKey, 
               remote_public_key: &PublicKey) -> TokenChannel {

        let balance = 0;
        let mutual_credit = MutualCredit::new(&local_public_key, &remote_public_key, balance);
        let rand_nonce = rand_nonce_from_public_key(&remote_public_key);

        // This is a special initialization case.
        // Note that this is the only case where new_token is not a valid signature.
        // We do this because we want to have synchronization between the two sides of the token
        // channel, however, the remote side has no means of generating the signature (Because he
        // doesn't have the private key). Therefore we use a dummy new_token instead.
        let first_move_token_lower = FriendMoveToken {
            operations: Vec::new(),
            old_token: token_from_public_key(&local_public_key),
            inconsistency_counter: 0, 
            move_token_counter: 0,
            balance: 0,
            local_pending_debt: 0,
            remote_pending_debt: 0,
            rand_nonce,
            new_token: token_from_public_key(&remote_public_key),
        };

        if sha_512_256(&local_public_key) < sha_512_256(&remote_public_key) {
            // We are the first sender
            let tc_outgoing = TcOutgoing {
                mutual_credit,
                move_token_out: first_move_token_lower,
                token_wanted: false,
                opt_prev_move_token_in: None,
            };
            TokenChannel {
                direction: TcDirection::Outgoing(tc_outgoing),
            }
        } else {
            // We are the second sender
            let tc_incoming = TcIncoming {
                mutual_credit,
                move_token_in: first_move_token_lower,
            };
            TokenChannel {
                direction: TcDirection::Incoming(tc_incoming),
            }
        }
    }

    pub fn new_from_remote_reset(local_public_key: &PublicKey, 
                      remote_public_key: &PublicKey, 
                      reset_move_token: &FriendMoveToken,
                      balance: i128) -> TokenChannel {

        let tc_incoming = TcIncoming {
            mutual_credit: MutualCredit::new(local_public_key, remote_public_key, balance),
            move_token_in: reset_move_token.clone(),
        };

        TokenChannel {
            direction: TcDirection::Incoming(tc_incoming),
        }
    }

    pub fn new_from_local_reset(local_public_key: &PublicKey, 
                      remote_public_key: &PublicKey, 
                      reset_move_token: &FriendMoveToken,
                      balance: i128,
                      opt_last_incoming_move_token: Option<FriendMoveToken>) -> TokenChannel {

        let tc_outgoing = TcOutgoing {
            mutual_credit: MutualCredit::new(local_public_key, remote_public_key, balance),
            move_token_out: reset_move_token.clone(),
            token_wanted: false,
            opt_prev_move_token_in: opt_last_incoming_move_token,
        };
        TokenChannel {
            direction: TcDirection::Outgoing(tc_outgoing),
        }
    }

    /// Get a reference to internal mutual_credit.
    pub fn get_mutual_credit(&self) -> &MutualCredit {
        match &self.direction {
            TcDirection::Incoming(tc_incoming) => &tc_incoming.mutual_credit,
            TcDirection::Outgoing(tc_outgoing) => &tc_outgoing.mutual_credit,
        }
    }

    pub fn get_remote_max_debt(&self) -> u128 {
        self.get_mutual_credit().state().balance.remote_max_debt
    }

    pub fn get_direction(&self) -> &TcDirection {
        &self.direction
    }

    /// Get the last incoming move token
    /// If no such incoming move token exists (Maybe this is the beginning of the relationship),
    /// returns None.
    pub fn get_last_incoming_move_token(&self) -> Option<&FriendMoveToken> {
        match &self.direction {
            TcDirection::Incoming(tc_incoming) => Some(&tc_incoming.move_token_in),
            TcDirection::Outgoing(tc_outgoing) => {
                match &tc_outgoing.opt_prev_move_token_in {
                    None => None,
                    Some(prev_move_token_in) => Some(prev_move_token_in),
                }
            },
        }
    }

    pub fn mutate(&mut self, d_mutation: &TcMutation) {
        match d_mutation {
            TcMutation::McMutation(mc_mutation) => {
                let mutual_credit = match &mut self.direction {
                    TcDirection::Incoming(tc_incoming) => &mut tc_incoming.mutual_credit,
                    TcDirection::Outgoing(tc_outgoing) => &mut tc_outgoing.mutual_credit,
                };
                mutual_credit.mutate(mc_mutation);
            },
            TcMutation::SetDirection(ref set_direction) => {
                self.direction = match set_direction {
                    SetDirection::Incoming(friend_move_token) => {
                        let tc_incoming = TcIncoming {
                            mutual_credit: self.get_mutual_credit().clone(), // TODO: Remove this clone()
                            move_token_in: friend_move_token.clone(), 
                        };
                        TcDirection::Incoming(tc_incoming)
                    },
                    SetDirection::Outgoing(friend_move_token) => {
                        let tc_outgoing = TcOutgoing {
                            mutual_credit: self.get_mutual_credit().clone(), // TODO; Remove this clone()
                            move_token_out: friend_move_token.clone(),
                            token_wanted: false,
                            opt_prev_move_token_in: self.get_last_incoming_move_token().cloned()
                        };
                        TcDirection::Outgoing(tc_outgoing)
                    }
                };
            },
            TcMutation::SetTokenWanted => {
                match self.direction {
                    TcDirection::Incoming(_) => unreachable!(),
                    TcDirection::Outgoing(ref mut tc_outgoing) => {
                        tc_outgoing.token_wanted = true;
                    },
                }
            },
        }
    }

    fn get_cur_move_token(&self) -> &FriendMoveToken {
        match &self.direction {
            TcDirection::Incoming(tc_incoming) => &tc_incoming.move_token_in,
            TcDirection::Outgoing(tc_outgoing) => &tc_outgoing.move_token_out,
        }
    }


    pub fn get_inconsistency_counter(&self) -> u64 {
        self.get_cur_move_token().inconsistency_counter
    }

    pub fn get_move_token_counter(&self) -> u128 {
        self.get_cur_move_token().move_token_counter
    }

    /// Get the current new token (Either incoming or outgoing)
    /// This is the most recent token in the chain.
    pub fn get_new_token(&self) -> &Signature {
        match &self.direction {
            TcDirection::Incoming(tc_incoming) => &tc_incoming.move_token_in.new_token,
            TcDirection::Outgoing(tc_outgoing) => &tc_outgoing.move_token_out.new_token,
        }
    }

    pub fn is_outgoing(&self) -> bool {
        match self.direction {
            TcDirection::Incoming(_) => false,
            TcDirection::Outgoing(_) => true,
        }
    }

    pub fn simulate_receive_move_token(&self, 
                              new_move_token: FriendMoveToken)
        -> Result<ReceiveMoveTokenOutput, ReceiveMoveTokenError> {

        match &self.direction {
            TcDirection::Incoming(tc_incoming) => {
                tc_incoming.handle_incoming(new_move_token)
            },
            TcDirection::Outgoing(tc_outgoing) => {
                tc_outgoing.handle_incoming(new_move_token)
            },
        }
    }
}


impl TcIncoming {
    /// Handle an incoming move token during Incoming direction:
    fn handle_incoming(&self, 
                        new_move_token: FriendMoveToken) 
        -> Result<ReceiveMoveTokenOutput, ReceiveMoveTokenError> {
        // We compare the whole move token message and not just the signature (new_token)
        // because we don't check the signature in this flow.
        if &self.move_token_in == &new_move_token {
            // Duplicate
            Ok(ReceiveMoveTokenOutput::Duplicate)
        } else {
            // Inconsistency
            Err(ReceiveMoveTokenError::ChainInconsistency)
        }
    }

    pub async fn create_friend_move_token(&self,
                                    operations: Vec<FriendTcOp>,
                                    rand_nonce: RandValue,
                                    identity_client: IdentityClient) -> FriendMoveToken {

        await!(FriendMoveToken::new(
            operations,
            self.move_token_in.new_token.clone(),
            self.move_token_in.inconsistency_counter,
            self.move_token_in.move_token_counter.wrapping_add(1),
            self.mutual_credit.state().balance.balance,
            self.mutual_credit.state().balance.local_pending_debt,
            self.mutual_credit.state().balance.remote_pending_debt,
            rand_nonce,
            identity_client))
    }

    pub fn begin_outgoing_move_token(&self) -> OutgoingMc {
        // TODO; Maybe take max_operations_in_batch as argument instead?
        OutgoingMc::new(&self.mutual_credit, MAX_OPERATIONS_IN_BATCH)
    }
}



impl TcOutgoing {
    /// Handle an incoming move token during Outgoing direction:
    fn handle_incoming(&self, 
                        new_move_token: FriendMoveToken) 
        -> Result<ReceiveMoveTokenOutput, ReceiveMoveTokenError> {

        // Verify signature:
        // Note that we only verify the signature here, and not at the Incoming part.
        // This allows the genesis move token to occur smoothly, even though its signature
        // is not correct.
        let remote_public_key = &self.mutual_credit.state().idents.remote_public_key;
        if !new_move_token.verify(remote_public_key) {
            return Err(ReceiveMoveTokenError::InvalidSignature);
        }

        // let friend_move_token = &tc_outgoing.move_token_out;
        if &new_move_token.old_token == &self.move_token_out.new_token {
            self.handle_incoming_token_match(new_move_token)
            // self.outgoing_to_incoming(friend_move_token, new_move_token)
        } else if self.move_token_out.old_token == new_move_token.new_token {
            // We should retransmit our move token message to the remote side.
            Ok(ReceiveMoveTokenOutput::RetransmitOutgoing(self.move_token_out.clone()))
        } else {
            Err(ReceiveMoveTokenError::ChainInconsistency)
        }
    }

    fn handle_incoming_token_match(&self,
                                   new_move_token: FriendMoveToken)
        -> Result<ReceiveMoveTokenOutput, ReceiveMoveTokenError> {
    
        // Verify counters:
        if new_move_token.inconsistency_counter != self.move_token_out.inconsistency_counter {
            return Err(ReceiveMoveTokenError::InvalidInconsistencyCounter);
        }

        let expected_move_token_counter = self.move_token_out.move_token_counter
            .checked_add(1)
            .ok_or(ReceiveMoveTokenError::MoveTokenCounterOverflow)?;

        if new_move_token.move_token_counter != expected_move_token_counter {
            return Err(ReceiveMoveTokenError::InvalidMoveTokenCounter);
        }

        let mut mutual_credit = self.mutual_credit.clone();
        let res = process_operations_list(&mut mutual_credit,
            new_move_token.operations.clone());

        // Verify balance:
        if mutual_credit.state().balance.balance != new_move_token.balance ||
           mutual_credit.state().balance.local_pending_debt != new_move_token.local_pending_debt ||
           mutual_credit.state().balance.remote_pending_debt != new_move_token.remote_pending_debt {
            return Err(ReceiveMoveTokenError::InvalidStatedBalance);
        }

        match res {
            Ok(outputs) => {
                let mut move_token_received = MoveTokenReceived {
                    incoming_messages: Vec::new(),
                    mutations: Vec::new(),
                };

                for output in outputs {
                    let ProcessOperationOutput 
                        {incoming_message, mc_mutations} = output;

                    if let Some(funds) = incoming_message {
                        move_token_received.incoming_messages.push(funds);
                    }
                    for mc_mutation in mc_mutations {
                        move_token_received.mutations.push(
                            TcMutation::McMutation(mc_mutation));
                    }
                }
                move_token_received.mutations.push(
                    TcMutation::SetDirection(SetDirection::Incoming(new_move_token)));
                Ok(ReceiveMoveTokenOutput::Received(move_token_received))
            },
            Err(e) => {
                Err(ReceiveMoveTokenError::InvalidTransaction(e))
            },
        }
    }



    /// Get the current outgoing move token
    pub fn create_outgoing_move_token_request(&self) -> FriendMoveTokenRequest {
        FriendMoveTokenRequest {
            friend_move_token: self.move_token_out.clone(),
            token_wanted: self.token_wanted,
        }
    }
}
