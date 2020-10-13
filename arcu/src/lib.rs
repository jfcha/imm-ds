pub mod arcu;
pub use arcu::*;


#[cfg(test)]
mod tests {
    use super::*;
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
        let v = Arcu::new(5);
        assert!(v.ref_count() == 1);
        let mut v2 = v.clone();
        assert!(v2.ref_count() == 2);
        assert!(v.ref_count() == 2);
        v2.update_value(10);
        assert!(5 == *v);
        assert!(v.ref_count() == 2);
        assert!(10 == *v2);
        assert!(v2.ref_count() == 2);
        assert!(v.ref_count() == 1);
        drop(v2);
        assert!(v.ref_count() == 1);
        let v2 = v.clone();
        assert!(v.ref_count() == 2);
        assert!(v2.ref_count() == 2);
        assert!(v.ref_count() == 1);
        drop(v);
        assert!(v2.ref_count() == 1);
        assert!(10 == *v2)
    }
}
