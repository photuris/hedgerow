//! Tests for the patched, vendored `meshcore-rs`.
//!
//! Everything runs in memory: a `CommandHandler` over an mpsc channel, no
//! transport, no hardware. Time is paused where a timeout matters.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::task::{Context, Wake, Waker};
use std::time::Duration;

use meshcore_rs::commands::CommandHandler;
use meshcore_rs::reader::MessageReader;
use meshcore_rs::{EventDispatcher, EventPayload, EventType, MeshCoreEvent};
use tokio::sync::mpsc::{self, Receiver};

/// Frame type byte of a contact URI reply.
const CONTACT_URI_FRAME: u8 = 11;

/// Takes the queued command and answers `Ok` the moment the sender's
/// pending enqueue wakes the receiver, before the sender resumes.
struct ImmediateResponder {
    rx: std::sync::Mutex<Receiver<Vec<u8>>>,
    dispatcher: Arc<EventDispatcher>,
}

impl Wake for ImmediateResponder {
    fn wake(self: Arc<Self>) {
        let frame = self.rx.lock().unwrap().try_recv().unwrap();

        assert_eq!(frame, vec![0x20]);
        futures::executor::block_on(self.dispatcher.emit(MeshCoreEvent::ok()));
    }
}

#[tokio::test(start_paused = true)]
async fn should_keep_reply_dispatched_during_enqueue() {
    let dispatcher = Arc::new(EventDispatcher::new());
    let reader = Arc::new(MessageReader::new(dispatcher.clone()));
    let (tx, rx) = mpsc::channel(64);
    let responder = Arc::new(ImmediateResponder {
        rx: std::sync::Mutex::new(rx),
        dispatcher: dispatcher.clone(),
    });
    let waker = Waker::from(responder.clone());
    let mut context = Context::from_waker(&waker);

    assert!(
        responder
            .rx
            .lock()
            .unwrap()
            .poll_recv(&mut context)
            .is_pending()
    );

    let mut observer = dispatcher.receiver();
    let command = CommandHandler::new(tx, dispatcher, reader);
    let result = command
        .send_multi(
            &[0x20],
            &[EventType::Ok, EventType::Error],
            Duration::from_secs(10),
        )
        .await;

    assert_eq!(
        observer.try_recv().unwrap().event_type,
        EventType::Ok,
        "the reply must really have been emitted"
    );
    assert!(
        result.is_ok(),
        "reply emitted during enqueue was lost: {result:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn should_time_out_when_send_queue_is_full() {
    let dispatcher = Arc::new(EventDispatcher::new());
    let reader = Arc::new(MessageReader::new(dispatcher.clone()));
    let (tx, _rx) = mpsc::channel(1);

    tx.send(vec![99]).await.unwrap();

    let command = CommandHandler::new(tx, dispatcher, reader);
    let result = tokio::time::timeout(
        Duration::from_secs(11),
        command.send_multi(
            &[0x20],
            &[EventType::Ok, EventType::Error],
            Duration::from_secs(10),
        ),
    )
    .await;

    assert!(
        result.is_ok(),
        "the 10-second deadline did not bound the enqueue: {result:?}"
    );
}

#[tokio::test]
async fn should_hex_encode_contact_uri_payload() {
    let dispatcher = Arc::new(EventDispatcher::new());
    let reader = MessageReader::new(dispatcher.clone());
    let mut rx = dispatcher.receiver();

    reader
        .handle_rx(vec![CONTACT_URI_FRAME, 0x00, 0xff, 0x10])
        .await
        .unwrap();

    let event = rx.try_recv().unwrap();

    assert_eq!(event.event_type, EventType::ContactUri);
    assert!(
        matches!(&event.payload, EventPayload::String(s) if s == "meshcore://00ff10"),
        "payload was {:?}",
        event.payload
    );
}
