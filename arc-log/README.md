# ArcLog

ArcLog is an append only, contiguous log that can be cloned (shallow) to provide multiple writers and
dereferencing to slices. Creating a slice (dereferencing) requires an atomic acquire, but otherwise
read performance should be on par with any other slice.

Pushing data occurs in a spin-lock (more options will available in the future) but should generally be very fast.
If capacity is reached, a new allocation will be created and all of the existing data will be copied (not moved) over to
the allocation. The Freeze bound and lack of &mut access any inner items should make copying safe. If a new
allocation occurs, the pushing ArcLog will be automatically updated to point to the new allocation. All other clones will still
point to the old allocation until they attempt to push or call update. It is the responsibility of each clone to
call update if they wish to see the new data that has been added. Once all clones are updated to the new
allocation the old allocation is deallocate. Drop on the inner items will only be called when the last clone
of ArcLog is dropped.



