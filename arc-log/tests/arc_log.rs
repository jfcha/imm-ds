//use super::ArcLog;

#[cfg(test)]
mod tests {
    use arc_log::ArcLog;
    use std::thread;
    use tracing_subscriber;
    use tracing::{event, Level, instrument};
    use core::sync::atomic::AtomicUsize;

    const TEST_LEVEL: Level = Level::DEBUG;

    #[derive(Debug)]
    struct DropTest(usize);

    impl Drop for DropTest {
        #[instrument]
        fn drop(&mut self) {
            event!(Level::TRACE, "Dropping test, value: {:?}", self.0);
        }
    }

    #[test]
    fn it_works() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(TEST_LEVEL)
            .with_test_writer()
            .try_init();
        let mut v = ArcLog::new();

        v.push_spin(DropTest(1));
        for i in 0..1 {
            event!(Level::TRACE, " {:?}", v[i]);
        }
        event!(Level::TRACE, " end data");
        v.push_spin(DropTest(2));
        for i in 0..2 {
            event!(Level::TRACE, " {:?}", v[i]);
        }
        event!(Level::TRACE, " end data");
        v.push_spin(DropTest(3));

        for i in 0..3 {
            event!(Level::TRACE, " {:?}", v[i]);
        }
        event!(Level::TRACE, " end data");
        v.push_spin(DropTest(4));
        for i in 0..4 {
            event!(Level::TRACE, " {:?}", v[i]);
        }
        event!(Level::TRACE, " end data");
        v.push_spin(DropTest(5));
        for i in 0..5 {
            event!(Level::TRACE, " {:?}", v[i]);
        }
        event!(Level::TRACE, " end data");
        v.push_spin(DropTest(6));
        for i in 0..6 {
            event!(Level::TRACE, " {:?}", v[i]);
        }
        event!(Level::TRACE, " end data");

        let av: &[_] = &*v;
        assert_eq!(av[1].0, 2);
    }

    #[test]
    fn it_works_2() {
        //use std::sync::Arc;
        let _ = tracing_subscriber::fmt()
            .with_max_level(TEST_LEVEL)
            .with_test_writer()
            .try_init();
        let mut v = ArcLog::new();

        v.push_spin(Box::new(AtomicUsize::new(1)));
        //v.push(2);
        //assert_eq!(v[1], 2);
    }

    #[test]
    fn clone_len() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(TEST_LEVEL)
            .with_test_writer()
            .try_init();
        let mut v = ArcLog::new();
        let mut v2 = v.clone();

        v.push_spin(DropTest(1));
        v.push_spin(DropTest(2));
        assert_eq!(v2.len(), 0);
        v2.update();
        assert_eq!(v2.len(), 2);
    }

    #[test]
    fn shared_data() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(TEST_LEVEL)
            .with_test_writer()
            .try_init();
        let mut copy_1 = ArcLog::new();
        event!(Level::TRACE, "Copy_1::new() : {:?}", copy_1);
        let mut copy_2 = copy_1.clone();
        event!(Level::TRACE, "Copy_2::clone() : {:?}", copy_2);
        copy_1.push_spin(1);
        event!(Level::TRACE, "Copy_1::push() : {:?}", copy_1);
        copy_2.push_spin(2);
        event!(Level::TRACE, "Copy_2::push() : {:?}", copy_2);
        copy_1.update();
        event!(Level::TRACE, "Copy_1::update() : {:?}", copy_1);
        assert_eq!(copy_1[1], 2);
        assert_eq!(copy_2[0], 1);
        let data = [1,2];
        assert_eq!(data, *copy_1);
        assert_eq!(data, *copy_2);
    }

    #[test]
    fn shared_mt_data() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(TEST_LEVEL)
            .with_test_writer()
            .try_init();
        let mut copy_1 = ArcLog::new();
        event!(Level::TRACE, "Copy_1::new() : {:?}", copy_1);
        let mut copy_2 = copy_1.clone();
        event!(Level::TRACE, "Copy_2::clone() : {:?}", copy_2);
        let handle = thread::spawn(move || {
            copy_2.push_spin(2)
        });
        
        let i1 = copy_1.push_spin(1);
        let i2 = handle.join().unwrap();
        copy_1.update();
        event!(Level::TRACE, "Copy_1::update() : {:?}", copy_1);
        assert_eq!(copy_1.len(), 2);
        assert_eq!(copy_1[i1], 1);
        assert_eq!(copy_1[i2], 2);
    }
    

    #[test]
    fn mt_test() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(TEST_LEVEL)
            .with_test_writer()
            .try_init();
        let mut v = ArcLog::new();
        let v2 = v.clone();
        let handle1 = thread::spawn(move || {
            let mut v2 = v2;
            for _i in 0..100 {
                v2.push_spin(1);
            }
        });
        let v2 = v.clone();
        let handle2 = thread::spawn(move || {
            let mut v2 = v2;
            for _i in 0..100 {
                v2.push_spin(2);
            }
        });
        let v2 = v.clone();
        let handle3 = thread::spawn(move || {
            let mut v2 = v2;
            for _i in 0..100 {
                v2.push_spin(3);
            }
        });
        let v2 = v.clone();
        let handle4 = thread::spawn(move || {
            let mut v2 = v2;
            for _i in 0..100 {
                v2.push_spin(4);
            }
        });
        for _i in 0..50 {
            v.push_spin(0);
        }
        handle1.join().unwrap();
        handle2.join().unwrap();
        handle3.join().unwrap();
        handle4.join().unwrap();
        v.update();
        let v_ref: &[i32] = &v;
        event!(Level::INFO, "values: {:?}", v_ref);
        assert_eq!(v.len(), 450);
        assert_eq!(v.iter().filter(|t| **t == 0).count(), 50);
        assert_eq!(v.iter().filter(|t| **t == 1).count(), 100);
        assert_eq!(v.iter().filter(|t| **t == 2).count(), 100);
        assert_eq!(v.iter().filter(|t| **t == 3).count(), 100);
        assert_eq!(v.iter().filter(|t| **t == 4).count(), 100);
    }
}