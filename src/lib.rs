pub mod updating_ref;


#[cfg(test)]
mod tests {
    use crate::updating_ref::*;
    #[test]
    fn it_works() {
        let v = Ref::new(5);
        let v1 = v.clone();
        let v2 = v.clone_to_updating();
        let v3 = v2.clone_to_ref();
        drop(v);
        drop(v1);
        drop(v2);
        assert!(5 == *v3)
    }

    #[test]
    fn update_doesnt_update_ref() {
        let v = Ref::new(5);
        let v2 = v.clone_to_updating();
        v2.update_to(10);
        drop(v2);
        assert!(5 == *v)
    }
    #[test]
    fn clone_update_drop_clone() {
        let v = Ref::new(5);
        assert!(v.ref_count() == 1);
        let v2 = v.clone_to_updating();
        assert!(v2.ref_count() == 2);
        assert!(v.ref_count() == 2);
        v2.update_to(10);
        assert!(5 == *v);
        assert!(v.ref_count() == 2);
        assert!(10 == *v2);
        assert!(v2.ref_count() == 2);
        assert!(v.ref_count() == 1);
        drop(v2);
        assert!(v.ref_count() == 1);
        let v2 = v.clone_to_updating();
        assert!(v.ref_count() == 2);
        assert!(v2.ref_count() == 2);
        assert!(v.ref_count() == 1);
        drop(v);
        assert!(v2.ref_count() == 1);
        assert!(10 == *v2)
    }
}
