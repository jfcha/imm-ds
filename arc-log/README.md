# ArcLog

ArcLog is an append only, contiguous log that can be cloned (shallow) to provide multiple read/writers that
dereference to slices. Creating a slice (dereferencing) requires an atomic acquire, but otherwise
read performance should be on par with any other slice.

Pushing data occurs in a spin-lock (more options will available in the future) but should generally be very fast.
If capacity is reached, a new underlying allocation will be created and all of the existing data will be copied (not moved) over to
a new allocation. By copying, existing reference remain valid. The Freeze bound and lack of &mut access 
any inner items allows the copying to be safe without the need for Copy, Clone, or intermediate drops. 
If a new allocation does occurs, the pushing ArcLog will be automatically updated to point to the new allocation. All other clones will still
point to the old allocation until they attempt to push or call update. It is the responsibility of each clone to
call update if they wish to see the new data on the new allocation. Once all clones are updated to the new
allocation, the old allocation is deallocated. Drop on the inner items will only once when the last clone
of ArcLog is dropped.

## Examples

Sharing in a single thread

```rust
use arc_log::ArcLog;

fn main(){
    let mut copy_1 = ArcLog::new();
    let mut copy_2 = copy_1.clone();

    copy_1.push(1);
    copy_2.push(2);
    copy_1.update();
    assert_eq!(copy_1[1], 2);
    assert_eq!(copy_2[0], 1);
    let data = [1,2];
    assert_eq!(data, *copy_1);
    assert_eq!(data, *copy_2);
}
```

Sharing in a multiple threads

```rust
use arc_log::ArcLog;

fn main(){
    let mut copy_1 = ArcLog::new();
    let mut copy_2 = copy_1.clone();

    copy_1.push(1);
    copy_2.push(2);
    copy_1.update();
    assert_eq!(copy_1[1], 2);
    assert_eq!(copy_2[0], 1);
    let data = [1,2];
    assert_eq!(data, *copy_1);
    assert_eq!(data, *copy_2);
}
```