use std::marker::Unpin;
use futures::{future, Stream, StreamExt, Sink};
use futures::task::Spawn;

use proto::funder::messages::{FunderToChanneler, ChannelerToFunder};
use crypto::identity::PublicKey;
use timer::TimerClient;

use utils::int_convert::usize_to_u64;

use relay::client::connector::{Connector, ConnPair};
use relay::client::client_listener::{client_listener, ClientListenerError};

use crate::listener::listener_loop;


fn inner_channeler_loop<FF,TF,C,A>(address: A,
                        from_funder: FF, 
                        to_funder: TF,
                        timer_client: TimerClient,
                        connector: C,
                        mut spawner: impl Spawn + Clone + Send + 'static)
where
    A: Clone,
    C: Connector<Address=A, SendItem=Vec<u8>, RecvItem=Vec<u8>>,
    FF: Stream<Item=FunderToChanneler<A>>,
    TF: Sink<SinkItem=ChannelerToFunder>,
{
    unimplemented!();
    // TODO:
    // Handle events in a loop:
    // - from Funder:
    //      - Message((PublicKey, Vec<u8>)), // (friend_public_key, message)
    //      - SetAddress(A), 
    //      - AddFriend((PublicKey, A)), // (friend_public_key, address)
    //      - RemoveFriend(PublicKey), // friend_public_key
    // - Incoming connection (from listener)
    // - Connection established (We initiated this connection)
}

