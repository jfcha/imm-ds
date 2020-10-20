# RCU

This repository is focused on bringing read, copy, update (RCU) inspired data structures to rust.

## Arcu

Arcu is an Arc with a forwarding address. This allows users to not only share data, but also update
when new data become available. By implementing Future and Stream, Arcu will allow 
async observability and updating of dependent data.

## ArcLog

ArcLog is an append only, contiguous log that can be updated by multiple writers and
dereferences to slices. Read performance should be on par with Vec.

