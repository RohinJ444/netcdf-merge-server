# NetCDF Merge Server

## Overview

This is a Rust/Rocket HTTP server that merges two NetCDF-4 files entirely in memory. A client uploads `part_a` and `part_b` under the same `name`, then calls `GET /read?name=<name>` to receive a new combined NetCDF-4 file.

The merge combines compatible NetCDF structure and data, including dimensions, global attributes, variable definitions, variable attributes, and primitive variable data. It does not currently try to solve problems like coordinate matching, timestep reconciliation, or regridding.

## Table of Contents

1. [Project Structure](#project-structure)
2. [API Endpoints](#api-endpoints)
3. [In-Memory Design](#in-memory-design)
4. [Merge Semantics](#merge-semantics)
5. [Parallelism](#parallelism)
6. [Running, Curl Usage, and Testing](#running-curl-usage-and-testing)
7. [Test Coverage](#test-coverage)
8. [Limitations and Future Directions](#limitations-and-future-directions)
9. [Resources and Acknowledgments](#resources-and-acknowledgments)

## Project Structure

```text
.
├── Cargo.toml
├── Cargo.lock
├── README.md
├── src
│   ├── main.rs
│   └── merge.rs
└── scripts
    ├── create_test_data.py
    └── run_integration_tests.py
```

### `src/main.rs`

`main.rs` contains the Rocket server setup, route handlers, upload validation, and persisted in-memory `HashMap`.

It defines four API routes:

- `GET /`
- `POST /part_a?name=<name>`
- `POST /part_b?name=<name>`
- `GET /read?name=<name>`

The central server state is a `HashMap` keyed by `name`. Each `name` maps to a `Parts` struct, which stores the current `part_a` bytes and current `part_b` bytes as optional `Vec<u8>` values. They are optional because a client can upload one side before the other. The map is wrapped in a Tokio `RwLock` so Rocket request handlers can share it while the server process is running.

Re-uploading one side under an existing name replaces that side of the pair. For example, if `part_a` and `part_b` have both been uploaded under `name=test`, a later upload to `POST /part_b?name=test` replaces only `part_b`; the existing `part_a` remains stored.

Before an upload is stored, `main.rs` validates that the bytes can be opened as NetCDF and that the file format is NetCDF-4 or NetCDF-4 classic. 

### `src/merge.rs`

`merge.rs` is the internal merge module called by `main.rs`. Its public entry point, `combine_netcdf4_in_memory`, takes the uploaded `part_a` and `part_b` byte slices and returns the merged NetCDF-4 bytes.

That function opens both inputs from memory, creates the output dataset in memory, copies dimensions and global attributes, defines output variables, leaves NetCDF define mode, then copies variable data into the output file. The direct NetCDF-C calls are kept inside private unsafe helpers, and the public function acquires a global lock before entering them, so the server code only needs to call one safe merge function.

I describe the lower-level memory flow in more detail in [In-Memory Design](#in-memory-design).

### `scripts/create_test_data.py`

`create_test_data.py` generates local NetCDF test files under `test_data/`. The generated files are only test inputs, so `test_data/` is ignored by Git.

### `scripts/run_integration_tests.py`

`run_integration_tests.py` tests the server through its HTTP API. It uploads the previously generated test files, calls `/read`, saves returned files when the merge should succeed, and uses Python's `netCDF4` package to inspect the returned NetCDF contents. For failure cases, it checks that the response is a `400` with plain-text error output. For a more detailed overview of the test cases in this script, see [Test Coverage](#test-coverage).

## API Endpoints

This section offers a brief summary of the available endpoints. For step-by-step commands to start the server, upload files, and download a merged output, see [Running, Curl Usage, and Testing](#running-curl-usage-and-testing).

| Method | Endpoint              | Purpose                                     |
| ------ | --------------------- | ------------------------------------------- |
| `GET`  | `/`                   | Check if server is running                  |
| `POST` | `/part_a?name=<name>` | Uploads the first NetCDF-4 file for `name`  |
| `POST` | `/part_b?name=<name>` | Uploads the second NetCDF-4 file for `name` |
| `GET`  | `/read?name=<name>`   | Returns the merged NetCDF-4 file for `name` |

`GET /read?name=<name>` returns `400 Bad Request` if the name does not exist, either side is missing, the uploads are not valid NetCDF-4 files, or the files fail under the merge rules outlined in [Merge Semantics](#merge-semantics).

## In-Memory Design

The main implementation constraint was to avoid disk I/O for the uploaded and merged NetCDF files: no saving uploaded files to temporary paths, and no creating the merged output as a temporary file before returning it.

Rust does not provide a NetCDF memory-merge API directly, so the merge uses NetCDF-C's in-memory API through `netcdf_sys`. That API supports the operations needed for the no-disk-I/O constraint: opening an existing NetCDF file from a memory buffer, creating a new output NetCDF file in memory, and retrieving the completed output buffer after closing it.

The NetCDF-C functions I used are:

- [`nc_open_mem`](https://docs.unidata.ucar.edu/netcdf-c/current/netcdf__mem_8h.html): opens a NetCDF file with contents taken from a memory block.
- [`nc_create_mem`](https://docs.unidata.ucar.edu/netcdf-c/current/netcdf__mem_8h.html): creates a NetCDF file with contents stored in memory.
- [`nc_close_memio`](https://docs.unidata.ucar.edu/netcdf-c/current/netcdf__mem_8h.html): closes an in-memory dataset and returns the final memory contents.

The algorithm for the merge is therefore:

1. Rocket reads each upload body into memory.
2. The upload handler stores each file as a `Vec<u8>` in the persisted `HashMap`.
3. `/read` retrieves the stored `part_a` and `part_b` byte vectors for the requested name.
4. `merge.rs` passes those byte buffers into `nc_open_mem`.
5. `merge.rs` creates the output dataset with `nc_create_mem`.
6. The merge code copies dimensions, attributes, variables, and data into the in-memory output dataset.
7. `nc_close_memio` returns the completed NetCDF output buffer.
8. Rust copies that C-owned buffer into a Rust-owned `Vec<u8>`, frees the C-owned memory, and returns the Rust vector through Rocket.

### Verifying No File Disk I/Os

After implementing and testing `merge.rs`, I wanted to verify that the server was actually meeting the no-disk-I/O constraint. I used macOS filesystem tracing while running the server and issuing upload/read requests:


```bash
sudo fs_usage -w -f filesys <server_pid_or_process_name>
sudo fs_usage -w -f diskio <server_pid_or_process_name>
```

None of the filesystem activity I saw was the server reading or writing uploaded NetCDF files or merged NetCDF output. Most of it was the operating system paging dynamic libraries and other already-installed library code into memory while the server and NetCDF-C/HDF5 stack were running.

However, one peculiar pattern in the logs was that the NetCDF-C/HDF5 stack occasionally made pathname/config probes during the memory-backed open/create calls. Those attempts failed with errors such as `ENOENT`, and the merge still proceeded through the buffers passed into `nc_open_mem` and `nc_create_mem`.

This is an artifact of the NetCDF-C memory API. The function signatures still include a `const char *path` parameter, even though `nc_open_mem` uses the caller-provided memory block as the file contents and `nc_create_mem` stores the created file in memory. In my code, I pass names like `memory_part_a`, `memory_part_b`, and `memory_combined` into that required `path` argument, and NetCDF-C stores that string as the name associated with the open dataset ID. My best read of the filesystem trace is that parts of the underlying stack still use that name during lower-level path/config probing, which would explain the failed `ENOENT` attempts. I did not trace this down to the specific call site in NetCDF-C/HDF5, so I am treating it as a hypothesis rather than a definitive internal explanation. I chose not to keep digging there because it would have meant stepping outside the server implementation and debugging the internals of large C/HDF5 dependencies, while the filesystem trace had already answered the question that mattered for this project: the uploaded and merged NetCDF files were not being read from or written to disk.

See the NetCDF-C [`netcdf_mem.h` reference](https://docs.unidata.ucar.edu/netcdf-c/current/netcdf__mem_8h.html) and [in-memory support documentation](https://docs.unidata.ucar.edu/netcdf-c/current/inmemory.html).

## Merge Semantics

A NetCDF dataset has three components that matter for this implementation:

1. Dimensions, which name and size axes like `time`, `lat`, or `lon`.
2. Variables, which hold typed data and refer to dimensions.
3. Attributes, which store metadata either globally for the whole file or locally on a specific variable.

This implementation merges each of those pieces directly, one at a time. It copies structure and data as they already exist in the input files; it does not align coordinate variables, reconcile timesteps, concatenate along dimensions, or regrid data.

### General merge restrictions

The server only accepts NetCDF-4 and NetCDF-4 classic input files. NetCDF-3, CDF-5, and any other file types are rejected at upload time.

Variables are copied as-is. The implementation supports primitive numeric NetCDF types, `NC_CHAR`, and `NC_STRING`. It does not support compound types, enum types, opaque types, variable-length arrays, groups, or other more complex NetCDF-4 features.

Same-named dimensions must have the same length. Different dimension names can coexist in the output file.

### Dimensions

For each source dimension, the merge reads the dimension's name and length. If the output file does not already have that dimension name, it defines a new output dimension. If the output already has that dimension name with the same length, it reuses the existing output dimension. If the output already has that dimension name with a different length, the merge fails.

This succeeds, as the dimension names and lengths are consistent across both files:

```text
part_a:
  time = 2
  lat = 4
  lon = 3

part_b:
  time = 2
  lat = 4
  lon = 3
```

This fails, as the dimension name is shared but the lengths disagree:

```text
part_a:
  time = 2

part_b:
  time = 5
```

### Global attributes

Global attributes are copied from `part_a` first, then from `part_b`. If both files have a global attribute with the same name, the first one copied is kept. In practice, duplicate global attributes keep the `part_a` value.

```text
part_a:
  title = "Test part A"

part_b:
  title = "Test part B"

combined:
  title = "Test part A"
```

### Variables and variable attributes

Variables are defined in the output file before their data is copied. For each variable, the merge copies the variable name, type, dimension references, and variable attributes.

Variables are copied from `part_a` first, then from `part_b`. If both files have a variable with the same name, the first copy is kept. In practice, duplicate variable names keep the `part_a` variable.

```text
part_a:
  temperature(time)

part_b:
  temperature(time)

combined:
  temperature(time)    # from part_a
```

Once all variables are defined, the output file leaves define mode. The merge then copies variable data.

For primitive numeric variables and `NC_CHAR`, the implementation calculates how many bytes are needed for the full variable, reads the source variable into a temporary byte buffer, and writes that buffer into the corresponding output variable.

`NC_STRING` variables use NetCDF-C's string-specific API instead. String values are not copied as one fixed-width byte buffer; the implementation reads the string pointers through `nc_get_var_string`, writes them through `nc_put_var_string`, and frees the C-allocated string memory with `nc_free_string`.

### Upload overwrite behavior

Uploads are keyed by `name`. If the same `name` is reused, the uploaded side is replaced.

For example, this sequence first returns a merge of `part_a.nc` and `part_b.nc`:

```text
POST /part_a?name=example   with part_a.nc
POST /part_b?name=example   with part_b.nc
GET  /read?name=example
```

If `part_b` is uploaded again under the same name, only `part_b` changes:

```text
POST /part_b?name=example   with overwrite_b.nc
GET  /read?name=example
```

The next `/read` returns `part_a.nc + overwrite_b.nc`.

## Parallelism

### Current synchronization model

The current implementation has two separate synchronization points.

First, the upload store is a persisted in-memory `HashMap` protected by a Tokio `RwLock`. That lets Rocket handle normal request-level concurrency: multiple requests can read from the store at the same time, while uploads that modify the store take write access.

Second, the NetCDF-C/HDF5 merge is serialized with a global `Mutex`. Before `combine_netcdf4_in_memory` enters the low-level merge code, it takes that lock, so only one merge can run inside the process at a time.

My decision to select that lock was based on the serialization model described in HDF5's [Thread Safe Library technical note](https://support.hdfgroup.org/releases/hdf5/v2_0/v2_0_0/documentation/doxygen/thread-safe-lib.html). In that model, a thread-safe build of HDF5 installs a recursive mutex around every library entry point, so only one thread is inside the library at a time. The default non-thread-safe build does not provide that protection. Since this server enters NetCDF-C/HDF5 through `netcdf-sys` and cannot assume how the linked HDF5 library was built, the Rust-side lock gives the merge code a consistent safety boundary before any NetCDF-C/HDF5 call.

NetCDF's built-in parallel I/O is a different mechanism. [NetCDF-4 parallel I/O](https://docs.unidata.ucar.edu/netcdf-c/4.9.3/parallel_io.html) is built on HDF5's parallel mode and uses MPI (Message Passing Interface) for coordinated multi-process access to one shared file. That model is useful for HPC jobs where multiple processes collectively write chunks of a large dataset. However, this server has a different problem: each request gives the server two complete in-memory files, and the question is how to handle multiple independent merges safely.

### What can be parallelized

The HTTP side is straightforwardly parallelizable. Rocket can accept multiple connections, read request bodies, validate uploads, store bytes by name, and return response bytes without entering NetCDF-C/HDF5.

The merge side is harder, and this is where HDF5's build configuration matters. HDF5, [documented by the HDF Group](https://confluence.hdfgroup.org/display/knowledge/Questions+about+thread-safety+and+concurrent+access), has three relevant build modes:

- Default builds are not thread-safe at all. Concurrent calls can corrupt internal state, including file ID tables and dimension/variable lookup structures. The HDF Group notes that the pre-built binaries available for download are not thread-safe, which means most general-purpose distributions fall into this bucket unless rebuilt.
- Thread-safe builds (`--enable-threadsafe`) install the recursive lock described above. Multiple threads can call into HDF5 safely, but only one is actually inside the library at a time. This is functionally equivalent to the explicit `Mutex` I am already using in Rust.
- Parallel builds (`--enable-parallel`) link against MPI for the HPC use case described above and are not applicable to this server.

So even a thread-safe HDF5 build does not give true concurrency for merges. The HDF Group's own [Toward Multi-Threaded Concurrency in HDF5](https://www.hdfgroup.org/wp-content/uploads/2022/05/Toward-MT-HDF5.pdf) document summarizes the current state plainly: the library is thread-safe but not concurrent. Work to change that is ongoing but is not part of any released version.

Parallelizing within one merge would be even more fragile. NetCDF writes happen in phases: define dimensions and variables, leave define mode, then write data. The output dataset is one shared NetCDF-C/HDF5 object, so parallelizing variable copies would require careful coordination around define mode, IDs, memory ownership, and output mutation order.

### What I would do next

The first improvement I would make is snapshotting by name. When `/read?name=foo` is called, the server should clone the current `part_a` and `part_b` bytes for that name, then release the upload-store lock before running the merge. That would make each read operate on a consistent pair of inputs while keeping the expensive merge work outside the store lock.

The next improvement would be per-name coordination. Uploads and reads for the same `name` should be coordinated, but unrelated names should not block each other at the server-state level. A read of `name=a` should not interfere with an upload to `name=b`.

For true parallel merges, I would use process isolation rather than thread-level parallelism inside one process. Rocket would keep handling HTTP, validation, and upload storage. When a merge is needed, it would send the two byte buffers to a bounded pool of worker processes. Each worker would run one NetCDF-C/HDF5 merge at a time and return the completed bytes. That gives real parallelism across workers without asking multiple Rust threads to share one NetCDF-C/HDF5 library state inside the same process, and it also bounds the impact of any library bug or memory leak to a single worker.

In Rust, that could be built with a small internal worker binary launched through [`std::process::Command`](https://doc.rust-lang.org/std/process/struct.Command.html), or with longer-running local workers communicating over pipes or sockets. A strictly weaker fallback would be [`tokio::task::spawn_blocking`](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html), but that would only keep Rocket's async runtime responsive. It would not give parallel merges as long as the global merge lock remains in place.

A longer-running version of this server would also need request size limits, an eviction policy for stale uploads, and a per-name TTL on the store. None of those are concurrency fixes by themselves, but they matter for any server that is expected to stay up.

## Running, Curl Usage, and Testing

These commands assume you are testing locally on port 8000.

### 1. Install Cargo, Python dependencies, and NetCDF command-line tools

Install Rust/Cargo from the official Rust installation page if you do not already have it:

```text
https://www.rust-lang.org/tools/install
```

Install the Python packages used by the test-data and integration-test scripts:

```bash
pip install netCDF4 numpy requests
```

If you are on macOS and want to inspect returned files with `ncdump`, install the NetCDF command-line tools with Homebrew:

```bash
brew install netcdf
```

### 2. Optional: generate Rust documentation

Rust can generate local HTML documentation from the rustdoc comments in `src/main.rs` and `src/merge.rs`:

```bash
cargo doc --no-deps --open
```

This builds documentation for this project without generating documentation pages for every dependency, and opens the generated HTML in your browser.

The generated documentation is written under `target/doc/`, which is ignored by Git.

### 3. Start the server

From the project root:

```bash
cargo run
```

The server should be available at:

```text
http://127.0.0.1:8000
```

Keep this terminal running.



### 4. Optional: try the server with your own files using curl

If you want to test the server manually with your own NetCDF-4 files, open a second terminal and replace the placeholder paths below with the files you want to upload.

Upload `part_a`:

```bash
curl -i -X POST \
  --data-binary @<path-to-part-a.nc> \
  "http://127.0.0.1:8000/part_a?name=test"
```

Upload `part_b`:

```bash
curl -i -X POST \
  --data-binary @<path-to-part-b.nc> \
  "http://127.0.0.1:8000/part_b?name=test"
```

Read the merged file:

```bash
curl -o <path-to-output-combined.nc> \
  "http://127.0.0.1:8000/read?name=test"
```

Inspect it with `ncdump`:

```bash
ncdump -h <path-to-output-combined.nc>
```

### 5. Generate test data files

From the project root:

```bash
python scripts/create_test_data.py
```

### 6. Run the integration tests

With the server still running locally on port 8000:

```bash
python scripts/run_integration_tests.py
```

## Test Coverage

The integration tests are endpoint-level tests. They call the server routes, then inspect either the returned NetCDF file or the returned error response.

The current test suite covers the following cases:

* `test_server_is_running`: confirms the Rocket health check responds.
* `test_standard_merge`: uploads two compatible files with the same dimensions but different variables and global attributes, then checks the returned dimensions, variables, variable attributes, and global attributes.
* `test_reupload_part_b_overwrites_existing_merge`: uploads an initial pair, checks the first merged file, re-uploads only `part_b` under the same name, then checks that the next merged file reflects the new `part_b` while keeping the old `part_a`.
* `test_dimension_conflict_returns_400`: uploads files with a same-named dimension of different lengths and checks that the merge fails clearly.
* `test_duplicate_variable_keeps_first`: uploads files with the same variable name and checks that the `part_a` version is kept while `part_b`-only global attributes are still copied.
* `test_disjoint_dimensions_merge`: uploads files with different dimension names and checks that both sets of dimensions and variables are preserved.
* `test_missing_part_b_returns_400`: uploads only `part_a` and checks that `/read` fails with the expected error.
* `test_missing_part_a_returns_400`: uploads only `part_b` and checks that `/read` fails with the expected error.
* `test_invalid_upload_returns_400`: uploads non-NetCDF bytes and checks that the upload is rejected.
* `test_netcdf3_upload_returns_400`: uploads a valid NetCDF-3 file and checks that it is rejected because the server expects NetCDF-4.
* `test_cdf5_upload_returns_400`: uploads a valid CDF-5 file and checks that it is rejected because the server expects NetCDF-4.
* `test_large_merge`: repeats the compatible merge path with larger generated files.
* `test_string_variables_merge`: uploads files with `NC_STRING` variables and checks that the returned file preserves the string variables, string attributes, and string values.

## Limitations and Future Directions

### Structural compatibility and scientific meaning

The merge currently requires same-named dimensions to have the same length. If `part_a` has `time = 2` and `part_b` has `time = 5`, the server does not concatenate, pad, or align the time dimension. It returns an error. Different dimension names can coexist, but the code does not infer that two differently named dimensions might represent the same conceptual axis.

The implementation also does not align coordinates, reconcile timesteps, regrid data, concatenate along unlimited dimensions, or detect semantic conflicts between coordinate variables. If both files contain a variable with the same name, the first one copied is kept. That behavior is simple and predictable, but it is not necessarily the optimal conflict-resolution strategy.

A more complete version could attempt to do these things:
* For concatenation, the server could allow a user to choose one dimension, such as `time`, as the concat dimension. It would then require every other shared dimension to match exactly, create the output `time` dimension with length `len(part_a.time) + len(part_b.time)`, and write each variable in two slices: the `part_a` values first, then the `part_b` values offset along the concat dimension. Variables that do not use the concat dimension could either be copied once or required to match exactly across both files.
* For coordinate alignment, the server could inspect coordinate variables like `time`, `lat`, or `lon` before copying data. If both files had the same coordinate values in the same order, it could copy directly. If the values were the same but ordered differently, it could reorder one file's data before writing. If the coordinate sets only partially overlapped, it would need a clear policy: take the union, take the intersection, or fail. That is more involved than checking dimension lengths because it requires comparing actual coordinate variable values, not just dimension metadata.
* For variable conflicts, the server could utilize a different policy instead of always keeping the first copy. For example, it could rename the second variable as `humidity_part_b`, fail on duplicate variable names, or require the duplicate variables to have identical values before keeping only one. The same kind of policy could apply to duplicate global attributes.
* Regridding is probably the most challenging thing to attempt as a simple extension because it is not just a NetCDF structural operation. To regrid correctly, the server would need to understand the coordinate reference system, grid topology, interpolation method, missing-value conventions, units, and whether the variable should be interpolated at all. For example, temperature might be interpolated differently from categorical masks or accumulated precipitation. That is a separate scientific-data-processing problem, not just a safer version of the current merge.

### NetCDF-4 feature coverage

The implementation currently supports primitive numeric variables, `NC_CHAR`, and `NC_STRING`. I chose that scope because those types cover the core fixed-size numeric arrays common in NetCDF files, plus string variables, while still keeping the merge implementation understandable and testable.

Primitive numeric variables and `NC_CHAR` can be copied through a direct byte-buffer path: calculate the element count, multiply by the element size, read the source variable into a temporary byte buffer, and write that buffer into the output variable. `NC_STRING` needs a separate string-specific path, but it is still contained: NetCDF-C provides `nc_get_var_string`, `nc_put_var_string`, and `nc_free_string`.

The unsupported types are different because they require copying type definitions or nested memory structures before the variable data itself can be copied:

- Compound types: these are struct-like user-defined types. Supporting them would require inspecting the compound type definition, recreating it in the output file, preserving field names and offsets, and handling any nested field types before defining variables that use the compound type.
- Enum types: these are named integer mappings. Supporting them would require copying the enum definition and its members into the output file, then mapping the source enum type ID to the new output type ID before defining enum variables.
- Opaque types: these store uninterpreted bytes with a defined size. Supporting them would require recreating the opaque type definition in the output file before copying variables of that type.
- Variable-length types: these cannot be handled as one flat byte buffer. Supporting them would require reading the variable-length descriptors, copying each element's data values, writing the output values, and freeing any C-managed memory correctly.
- Groups: this implementation only operates at the root dataset level. Supporting groups would require recursively walking the group hierarchy and recreating dimensions, attributes, types, and variables inside each output group.
- Compression/chunking and lower-level HDF5 layout details: the current merge focuses on logical NetCDF structure and data, not preserving every storage-layout property from the source files.

### Server lifecycle and memory management

Uploaded file pairs are stored in memory without expiration. That doesn't pose any issues for me, but at scale or in production I would have to consider things like request size limits and/or rate limits, cleanup for old names, and a more explicit memory budget. 

To address these considerations, a production version could expire old names after a fixed time window, track current memory usage, and reject new uploads once the server is near its memory budget.

### Concurrency

The current server allows normal request-level concurrency, but it serializes the NetCDF-C/HDF5 merge itself. That is conservative and safer for this implementation, but it means one large merge can block other merges.

A more advanced version or a production version would need a more deliberate concurrency model, especially per-name coordination for upload/read consistency and process-level isolation for higher-throughput merges. For more detail, see [Parallelism](#parallelism).

### NetCDF-C/HDF5 path probes

The in-memory merge still produced a few failed NetCDF-C/HDF5 path/config lookup attempts in filesystem tracing. These did not read or write the uploaded or merged NetCDF files, but they are worth noting because the trace is not completely silent.

For the full explanation, see [In-Memory Design](#in-memory-design).

## Resources and Acknowledgments

### Documentation

* [Rocket Programming Guide](https://rocket.rs/guide)
* [Rocket API documentation](https://docs.rs/rocket)
* [Rust installation / Cargo](https://www.rust-lang.org/tools/install)
* [Rust `std::process::Command`](https://doc.rust-lang.org/std/process/struct.Command.html)
* [Tokio `spawn_blocking`](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html)
* [NetCDF-C file and data I/O documentation](https://docs.unidata.ucar.edu/netcdf-c/current/group__datasets.html)
* [NetCDF-C `nc_open` documentation](https://docs.unidata.ucar.edu/netcdf-c/4.9.3/group__datasets.html#ga019098e9d5265006a11c9a841eb81b74)
* [NetCDF-C in-memory support documentation](https://docs.unidata.ucar.edu/netcdf-c/current/inmemory.html)
* [NetCDF-C `netcdf_mem.h` reference](https://docs.unidata.ucar.edu/netcdf-c/current/netcdf__mem_8h.html)
* [NetCDF-C parallel I/O documentation](https://docs.unidata.ucar.edu/netcdf-c/4.9.3/parallel_io.html)
* [NetCDF Users Guide](https://docs.unidata.ucar.edu/nug/current/)
* [netCDF4 Python documentation](https://unidata.github.io/netcdf4-python/)
* [Rust `netcdf` crate dependencies](https://crates.io/crates/netcdf/0.12.0/dependencies)
* [HDF5 thread-safe library documentation](https://support.hdfgroup.org/releases/hdf5/v2_0/v2_0_0/documentation/doxygen/thread-safe-lib.html)
* [HDF5 multi-threading RFC](https://support.hdfgroup.org/releases/hdf5/documentation/rfc/RFC_multi_thread.pdf)
* [HDF Group thread-safety and concurrent access FAQ](https://confluence.hdfgroup.org/display/knowledge/Questions+about+thread-safety+and+concurrent+access)
* [Toward Multi-Threaded Concurrency in HDF5](https://www.hdfgroup.org/wp-content/uploads/2022/05/Toward-MT-HDF5.pdf)
* [PnetCDF](https://parallel-netcdf.github.io/)

### YouTube

* [Introducing NetCDF and the CF and ACDD conventions](https://www.youtube.com/watch?v=FGHJhAFf1W0)
* [Async Rust explained in 20 minutes](https://www.youtube.com/watch?v=wXtngLBkK4Q&t=12s)
* [Rocket - The Rust Web Framework - Hello World](https://www.youtube.com/watch?v=EbU48bdVC60)
* [Rust for Dummies in 12 Minutes](https://www.youtube.com/watch?v=0y6RKiIk6cs)

### Additional tools

I also leveraged ChatGPT and Codex while working through Rust syntax, Rocket routing, NetCDF-C calls, HDF5 behavior, comments, tests, and README structure, mainly for debugging, explanation, and drafting support.

This was a fun and rewarding challenge, overall. I had not worked with Rust or Rocket before, and I had not previously used NetCDF-C's memory API directly. Building this forced me to move between examples, docs, filesystem traces until the pieces fit together. 
