use futures::StreamExt;
use wlan_common::timer::{self, TimeoutDuration};
use zx::{MonotonicDuration, MonotonicInstant};

#[derive(Clone, Copy)]
struct TestEvent(u8);

impl TimeoutDuration for TestEvent {
    fn timeout_duration(&self) -> MonotonicDuration {
        MonotonicDuration::from_millis(i64::from(self.0))
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
}

#[test]
fn deadlines_fire_in_order_and_keep_event_ids() {
    runtime().block_on(async {
        let (mut timer, stream) = timer::create_timer();
        let mut stream = timer::make_async_timed_event_stream(stream);
        let now = MonotonicInstant::get();
        let _late = timer.schedule_at(now + MonotonicDuration::from_millis(30), TestEvent(30));
        let early = timer.schedule_at(now + MonotonicDuration::from_millis(2), TestEvent(2));
        let _middle = timer.schedule_at(now + MonotonicDuration::from_millis(15), TestEvent(15));

        let first = stream.next().await.unwrap();
        let second = stream.next().await.unwrap();
        let third = stream.next().await.unwrap();
        assert_eq!([first.event.0, second.event.0, third.event.0], [2, 15, 30]);
        assert_eq!(first.id, early.id());
    });
}

#[test]
fn dropped_handles_cancel_and_drop_without_cancel_preserves() {
    runtime().block_on(async {
        let (mut timer, stream) = timer::create_timer();
        let mut stream = timer::make_async_timed_event_stream(stream);
        let now = MonotonicInstant::get();
        let canceled = timer.schedule_at(now + MonotonicDuration::from_millis(1), TestEvent(1));
        drop(canceled);
        timer
            .schedule_at(now + MonotonicDuration::from_millis(3), TestEvent(3))
            .drop_without_cancel();

        assert_eq!(stream.next().await.unwrap().event.0, 3);
    });
}
