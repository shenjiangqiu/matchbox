mod messages;
mod signal_peer;
mod socket;

pub(crate) mod error;
use std::pin::Pin;

use crate::Error;
use async_trait::async_trait;
use cfg_if::cfg_if;
use futures::{Future, FutureExt, StreamExt};
use futures_util::select;
use log::{debug, warn};
use messages::*;
pub(crate) use socket::MessageLoopChannels;
pub use socket::{ChannelConfig, RtcIceServerConfig, WebRtcSocket, WebRtcSocketConfig};

use self::error::SignallingError;

cfg_if! {
    if #[cfg(target_arch = "wasm32")] {
        mod wasm;
        use wasm::message_loop::message_loop;
        type UseSignaller = wasm::WasmSignaller;
        type MessageLoopFuture = Pin<Box<dyn Future<Output = Result<(), Error>>>>;
    } else {
        mod native;
        use native::message_loop::message_loop;
        type UseSignaller = native::NativeSignaller;
        type MessageLoopFuture = Pin<Box<dyn Future<Output = Result<(), Error>> + Send>>;
    }
}

// TODO: Should be a WebRtcConfig field
/// The duration, in milliseconds, to send "Keep Alive" requests
const KEEP_ALIVE_INTERVAL: u64 = 10_000;

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
trait Signaller: Sized {
    async fn new(room_url: &str) -> Result<Self, SignallingError>;

    async fn send(&mut self, request: String) -> Result<(), SignallingError>;

    async fn next_message(&mut self) -> Result<String, SignallingError>;
}

async fn signalling_loop<S: Signaller>(
    room_url: String,
    mut requests_receiver: futures_channel::mpsc::UnboundedReceiver<PeerRequest>,
    events_sender: futures_channel::mpsc::UnboundedSender<PeerEvent>,
) -> Result<(), SignallingError> {
    let mut signaller = S::new(&room_url).await?;

    loop {
        select! {
            request = requests_receiver.next().fuse() => {
                let request = serde_json::to_string(&request).expect("serializing request");
                debug!("-> {request}");
                signaller.send(request).await.map_err(SignallingError::from)?;
            }

            message = signaller.next_message().fuse() => {
                match message {
                    Ok(message) => {
                        debug!("Received {message}");
                        let event: PeerEvent = serde_json::from_str(&message)
                            .unwrap_or_else(|err| panic!("couldn't parse peer event: {}.\nEvent: {}", err, message));
                        events_sender.unbounded_send(event).map_err(SignallingError::from)?;
                    }
                    Err(SignallingError::UnknownFormat) => warn!("ignoring unexpected non-text message from signalling server"),
                    Err(err) => break Err(err)
                }

            }

            complete => break Ok(())
        }
    }
}

/// The raw format of data being sent and received.
type Packet = Box<[u8]>;
