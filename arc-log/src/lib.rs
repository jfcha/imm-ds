#![feature(allocator_api, slice_ptr_get, try_reserve, optin_builtin_traits, negative_impls)]

pub mod arc_log;
pub use arc_log::*;


#[cfg(test)]
mod tests {
    use super::*;
    //    use tracing_subscriber;

    #[derive(Debug)]
    struct DropTest(usize);

    impl Drop for DropTest {
        //        #[instrument]
        fn drop(&mut self) {
            eprintln!("Dropping test, value: {:?}", self.0);
            //            event!(Level::TRACE, "trying to drop");
        }
    }

    #[test]
    fn it_works() {
        //tracing_subscriber::fmt().with_max_level(Level::TRACE).init();
        let mut v = ArcLog::new();

        v.push(DropTest(1));
        v.push(DropTest(2));
        let av: &[_] = &*v;
        assert_eq!(av[1].0, 2);
    }

    #[test]
    fn it_works_2() {
        //tracing_subscriber::fmt().with_max_level(Level::TRACE).init();
        let mut v = ArcLog::new();

        v.push(1);
        v.push(2);
        assert_eq!(v[1], 2);
    }

    #[test]
    fn clone_len() {
        //tracing_subscriber::fmt().with_max_level(Level::TRACE).init();
        let mut v = ArcLog::new();
        let mut v2 = v.clone();

        v.push(DropTest(1));
        v.push(DropTest(2));
        assert_eq!(v2.len(), 0);
        v2.update();
        assert_eq!(v2.len(), 2);
    }
}
