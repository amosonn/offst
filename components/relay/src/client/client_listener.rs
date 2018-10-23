use std::marker::Unpin;

use futures::{future, Future, FutureExt, TryFutureExt, stream, Stream, StreamExt, Sink, SinkExt};
use futures::task::{Spawn, SpawnExt};
use futures::channel::mpsc;

use crypto::identity::PublicKey;
use proto::relay::messages::{InitConnection, RelayListenOut, RelayListenIn, 
    IncomingConnection, RejectConnection};
use proto::relay::serialize::{serialize_init_connection,
    serialize_relay_listen_in, deserialize_relay_listen_out};

use timer::TimerClient;
use super::connector::{Connector, ConnPair};
use super::access_control::{AccessControl, AccessControlOp};


#[derive(Debug)]
pub enum ClientListenerError {
    RequestTimerStreamError,
    SendInitConnectionError,
    ConnectionFailure,
    ConnectionTimedOut,
    TimerClosed,
    AccessControlError,
    SendToServerError,
    ServerClosed,
    SpawnError,
}


#[derive(Debug, Clone)]
enum ClientListenerEvent {
    TimerTick,
    TimerClosed,
    AccessControlOp(AccessControlOp),
    ServerMessage(RelayListenOut),
    ServerClosed,
    PendingReject(PublicKey),
}

#[derive(Debug)]
enum AcceptConnectionError {
    ConnectionFailed,
    PendingRejectSenderError,
    SendInitConnectionError,
    SendConnPairError,
}

async fn accept_connection<CS, CSE>(public_key: PublicKey, 
                           fut_conn_pair: impl Future<Output=Option<ConnPair<Vec<u8>, Vec<u8>>>>,
                           mut pending_reject_sender: mpsc::Sender<PublicKey>,
                           mut connections_sender: CS) -> Result<(), AcceptConnectionError> 
where
    CS: Sink<SinkItem=ConnPair<Vec<u8>, Vec<u8>>, SinkError=CSE> + Unpin + 'static,
{

    let mut conn_pair = match await!(fut_conn_pair) {
        Some(conn_pair) => conn_pair,
        None => {
            // Notify about connection failure:
            await!(pending_reject_sender.send(public_key))
                .map_err(|_| AcceptConnectionError::PendingRejectSenderError)?;
            return Err(AcceptConnectionError::ConnectionFailed);
        },
    };
    let ser_init_connection = serialize_init_connection(&InitConnection::Accept(public_key));
    await!(conn_pair.sender.send(ser_init_connection))
        .map_err(|_| AcceptConnectionError::SendInitConnectionError)?;

    await!(connections_sender.send(conn_pair))
        .map_err(|_| AcceptConnectionError::SendConnPairError)?;
    Ok(())
}


async fn inner_client_listener<C,IAC,CS,CSE>(mut connector: C,
                                incoming_access_control: IAC,
                                connections_sender: CS,
                                keepalive_ticks: usize,
                                mut timer_client: TimerClient,
                                mut spawner: impl Spawn,
                                mut opt_event_sender: Option<mpsc::Sender<ClientListenerEvent>>) 
    -> Result<(), ClientListenerError>
where
    C: Connector<Address=(), SendItem=Vec<u8>, RecvItem=Vec<u8>> + Clone + Send,
    IAC: Stream<Item=AccessControlOp> + Unpin,
    CS: Sink<SinkItem=ConnPair<Vec<u8>, Vec<u8>>, SinkError=CSE> + Unpin + Clone + Send,
{
    let timer_stream = await!(timer_client.request_timer_stream())
        .map_err(|_|  ClientListenerError::RequestTimerStreamError)?;

    let conn_pair = match await!(connector.connect(())) {
        Some(conn_pair) => conn_pair,
        None => return Err(ClientListenerError::ConnectionFailure),
    };

    // A channel used by the accept_connection.
    // In case of failure to accept a connection, the public key of the rejected remote host will
    // be received at pending_reject_receiver
    let (pending_reject_sender, pending_reject_receiver) = mpsc::channel::<PublicKey>(0);

    let ConnPair {mut sender, receiver} = conn_pair;
    let ser_init_connection = serialize_init_connection(&InitConnection::Listen);

    await!(sender.send(ser_init_connection))
        .map_err(|_| ClientListenerError::SendInitConnectionError)?;

    // Add serialization for sender:
    let mut sender = sender
        .sink_map_err(|_| ())
        .with(|vec| -> future::Ready<Result<_,()>> {
            future::ready(Ok(serialize_relay_listen_in(&vec)))
        });

    // Add deserialization for receiver:
    let receiver = receiver.map(|relay_listen_out| {
        deserialize_relay_listen_out(&relay_listen_out).ok()
    }).take_while(|opt_relay_listen_out| {
        future::ready(opt_relay_listen_out.is_some())
    }).map(|opt| opt.unwrap());

    let mut access_control = AccessControl::new();
    // Amount of ticks remaining until we decide to close this connection (Because remote is idle):
    let mut ticks_to_close = keepalive_ticks;
    // Amount of ticks remaining until we need to send a new keepalive (To make sure remote side
    // knows we are alive).
    let mut ticks_to_send_keepalive = keepalive_ticks / 2;


    let timer_stream = timer_stream
        .map(|_| ClientListenerEvent::TimerTick)
        .chain(stream::once(future::ready(ClientListenerEvent::TimerClosed)));

    let incoming_access_control = incoming_access_control
        .map(|access_control_op| ClientListenerEvent::AccessControlOp(access_control_op));

    let server_receiver = receiver
        .map(ClientListenerEvent::ServerMessage)
        .chain(stream::once(future::ready(ClientListenerEvent::ServerClosed)));

    let pending_reject_receiver = pending_reject_receiver
        .map(ClientListenerEvent::PendingReject);

    let mut events = timer_stream
        .select(incoming_access_control)
        .select(server_receiver)
        .select(pending_reject_receiver);

    while let Some(event) = await!(events.next()) {
        if let Some(ref mut event_sender) = opt_event_sender {
            await!(event_sender.send(event.clone()));
        }
        match event {
            ClientListenerEvent::TimerTick => {
                ticks_to_close = ticks_to_close.saturating_sub(1);
                ticks_to_send_keepalive = ticks_to_send_keepalive.saturating_sub(1);
                if ticks_to_close == 0 {
                    break;
                }
                if ticks_to_send_keepalive == 0 {
                    await!(sender.send(RelayListenIn::KeepAlive))
                        .map_err(|_| ClientListenerError::SendToServerError)?;
                    ticks_to_send_keepalive = keepalive_ticks / 2;
                }
            },
            ClientListenerEvent::TimerClosed => return Err(ClientListenerError::TimerClosed),
            ClientListenerEvent::AccessControlOp(access_control_op) => {
                access_control.apply_op(access_control_op)
                    .map_err(|_| ClientListenerError::AccessControlError)?;
            },
            ClientListenerEvent::ServerMessage(relay_listen_out) => {
                ticks_to_close = keepalive_ticks;
                match relay_listen_out {
                    RelayListenOut::KeepAlive => {},
                    RelayListenOut::IncomingConnection(IncomingConnection(public_key)) => {
                        if !access_control.is_allowed(&public_key) {
                            await!(sender.send(RelayListenIn::RejectConnection(RejectConnection(public_key))))
                                .map_err(|_| ClientListenerError::SendToServerError)?;
                            ticks_to_send_keepalive = keepalive_ticks / 2;
                        } else {
                            // We will attempt to accept the connection
                            let mut c_connector = connector.clone();
                            let fut_conn_pair = async move {await!(c_connector.connect(()))};
                            let fut_accept = accept_connection(
                                public_key,
                                fut_conn_pair,
                                pending_reject_sender.clone(),
                                connections_sender.clone())
                            .map_err(|e| {
                                error!("Error in accept_connection: {:?}", e);
                            }).map(|_| ());
                            spawner.spawn(fut_accept)
                                .map_err(|_| ClientListenerError::SpawnError)?;
                        }
                    }
                }
            },
            ClientListenerEvent::PendingReject(public_key) => {
                await!(sender.send(RelayListenIn::RejectConnection(RejectConnection(public_key))))
                    .map_err(|_| ClientListenerError::SendToServerError)?;
                ticks_to_send_keepalive = keepalive_ticks / 2;
            },
            ClientListenerEvent::ServerClosed => return Err(ClientListenerError::ServerClosed),
        }
    }
    Ok(())
}
