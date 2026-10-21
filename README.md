# MPITK - MPI Based Filesystem Toolkit

**⚠️ These are experimental utilities not yet proven in production ⚠️**

## `mpitk-ls`

Build with Cargo and an installed MPI implementation:

```sh
cargo build --release --bin mpitk-ls
mpirun -n 4 target/release/mpitk-ls /path/to/directory -print
```

The output is tab-separated and includes entry type, byte size, human-readable
size, modification time in Unix seconds, and filepath. Use `-dump <file>` to
write the same output to a file instead of stdout.

Optional partitioning adds a `bundle_id` column:

```sh
mpirun -n 4 target/release/mpitk-ls /path/to/directory -bundle-files 100 -dump files.tsv
mpirun -n 4 target/release/mpitk-ls /path/to/directory -bundle-size 1GiB -print
```

Bundles are assigned greedily in filepath order. `-bundle-files N` limits each
bundle to N files. `-bundle-size SIZE` limits the aggregate stat-reported size;
an individual file larger than the limit is assigned its own bundle. Sizes
accept bytes and units such as `KB`, `MB`, `KiB`, and `GiB`. Regular files and
symlinks receive bundle IDs; directory rows use `-`.
