
#[cfg(test)]
mod tests {
    //use arc_log::ArcLog;
    use core::sync::atomic::AtomicUsize;
    use std::thread;
    use arcu::Arcu;
    use tracing::{event, instrument, Level, field::debug};
    use tracing_subscriber;

    const TEST_LEVEL: Level = Level::TRACE;

    #[derive(Debug)]
    struct DropTest(usize);

    impl Drop for DropTest {
        #[instrument]
        fn drop(&mut self) {
            //event!(Level::TRACE, "Dropping test, value: {:?}", self.0);
        }
    }

#[test]
fn it_works() {
    let v = Arcu::new(5);
    let v1 = v.clone();
    drop(v);
    assert!(5 == *v1)
}

#[test]
fn update_doesnt_update_ref() {
    let mut v = Arcu::new(5);
    v.update_value(10);
    assert!(5 == *v);
    v.update();
    assert!(10 == *v)
}

#[test]
fn clone_update_drop_clone() {
    let _ = tracing_subscriber::fmt()
            .with_max_level(TEST_LEVEL)
            .with_test_writer()
            .try_init();
    let v = Arcu::new(5);
    event!(Level::TRACE, "1 v: {:?}", v);
    assert!(v.ref_count() == 1);
    let mut v2 = v.clone();
    event!(Level::TRACE, "2 v: {:?}", v);
    assert!(v2.ref_count() == 2);
    event!(Level::TRACE, "3 v2: {:?}", v2);
    assert!(v.ref_count() == 2);
    v2.update_value(10);
    event!(Level::TRACE, "4 v2: {:?}", v2);
    assert!(5 == *v);
    event!(Level::TRACE, "5 v: {:?}", v);
    assert!(v.ref_count() == 2);
    v2.update();
    event!(Level::TRACE, "6 v2: {:?}", v2);
    assert!(10 == *v2);
    assert!(v2.ref_count() == 2);
    event!(Level::TRACE, "7 v: {:?}", v);
    assert!(v.ref_count() == 1);
    drop(v2);
    event!(Level::TRACE, "8 v: {:?}", v);
    assert!(v.ref_count() == 1);
    let mut v2 = v.clone();
    event!(Level::TRACE, "9 v: {:?}", v);
    assert!(v.ref_count() == 2);
    event!(Level::TRACE, "10 v2: {:?}", v2);
    assert!(v2.ref_count() == 2);
    drop(v);
    event!(Level::TRACE, "11 v2: {:?}", v2);
    assert!(v2.ref_count() == 1);
    v2.update();
    event!(Level::TRACE, "1 v: {:?}", v2);
    assert!(10 == *v2);
    assert!(v2.ref_count() == 1);
}
}