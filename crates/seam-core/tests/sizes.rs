//! The sizes the docs quote, so that they cannot go stale: seam's
//! `io::Error` against std's, and the layer a root adds to an async core.
#![cfg(target_pointer_width = "64")]

use core::mem::size_of;

use seam_core::io;

#[test]
fn io_error_is_four_words_with_std() {
    // A kind, a `&'static str`, and with std the `std::io::Error` it came
    // from.
    let expected = if cfg!(feature = "std") { 32 } else { 24 };
    assert_eq!(size_of::<io::Error>(), expected);
    assert_eq!(size_of::<io::Result<()>>(), expected);
    assert_eq!(size_of::<io::Result<usize>>(), expected);
}

#[cfg(feature = "time")]
mod root {
    use core::mem::size_of_val;

    use seam_core::future::{AsyncPerform, SendPerform};
    use seam_core::ops::time::{Instant, Now};
    use seam_core::{Capability, Inspect, effects};

    struct Clock;
    impl Capability for Clock {}
    impl Inspect<Now> for Clock {
        fn inspect(&self, _: Now) -> Instant {
            Instant::EPOCH
        }
    }

    async fn plain<C: AsyncPerform<Now>>(c: &mut C) -> Instant {
        c.perform_async(Now).await
    }

    async fn send<C: SendPerform<Now> + Send>(c: &mut C) -> Instant {
        c.perform_send(Now).await
    }

    #[test]
    fn the_plain_async_forms_add_a_layer() {
        let mut root = seam_core::root![Clock<effects::Now>];
        assert_eq!(
            size_of_val(&plain(&mut root)),
            size_of_val(&plain(&mut Clock)) + 16
        );
    }

    #[test]
    fn the_send_forms_add_nothing() {
        let mut root = seam_core::root![Clock<effects::Now>];
        assert_eq!(size_of_val(&send(&mut root)), size_of_val(&send(&mut Clock)));
    }
}
