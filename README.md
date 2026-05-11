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
8. [Limitations](#limitations)
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

`main.rs` contains the Rocket server setup, route handlers, upload validation, and shared in-memory upload store.

It defines four API routes:

* `GET /`
* `POST /part_a?name=<name>`
* `POST /part_b?name=<name>`
* `GET /read?name=<name>`

Uploaded files are stored as `Vec<u8>` values in a `HashMap` keyed by `name`. Each name has one optional `part_a` and one optional `part_b`. The map is wrapped in a Tokio `RwLock` so the Rocket handlers can use it as shared application state while the server is running.

Re-uploading one side under an existing name replaces that side of the pair. For example, if `part_a` and `part_b` have both been uploaded under `name=test`, a later upload to `POST /part_b?name=test` replaces only `part_b`; the existing `part_a` remains stored.

Before an upload is stored, `main.rs` validates that the bytes can be opened as NetCDF and that the file format is NetCDF-4 or NetCDF-4 classic. Non-NetCDF files, NetCDF-3 files, and CDF-5 files are rejected before they enter the store.

### `src/merge.rs`

`merge.rs` is the internal merge implementation used by `main.rs`. I kept it separate so the Rocket request handling stays separate from the low-level NetCDF-C work.

The file crosses from Rust into NetCDF-C through Rust's foreign function interface (FFI). The public entry point, `combine_netcdf4_in_memory`, is safe to call from the rest of the server. It acquires the global merge lock, then calls a private unsafe helper where the direct NetCDF-C calls happen.

The unsafe helper uses three NetCDF-C memory functions. `nc_open_mem` opens the two uploaded files from caller-provided memory buffers. `nc_create_mem` creates the merged output file in memory. `nc_close_memio` closes the output dataset and returns the completed output memory buffer. Since that returned buffer is owned by C, the Rust code copies it into a Rust-owned `Vec<u8>` and then frees the original C-owned memory.

The rest of `merge.rs` is made up of smaller helper functions for copying dimensions, global attributes, variable definitions, variable attributes, and variable data. Those helpers also keep NetCDF-C pointer setup and status-code error handling out of the main merge flow.

I considered naming this file `merge_utils.rs`, but I think `merge.rs` is more accurate. It is not just miscellaneous utility code; it contains the actual merge path plus the helpers needed to keep that path readable.

### `scripts/create_test_data.py`

`create_test_data.py` generates local NetCDF fixtures under `test_data/`. The generated files are only test inputs, so `test_data/` is ignored by Git.

### `scripts/run_integration_tests.py`

`run_integration_tests.py` tests the server through its HTTP API. It uploads generated fixtures, calls `/read`, saves returned files when the merge should succeed, and uses Python's `netCDF4` package to inspect the returned NetCDF contents. For failure cases, it checks that the response is a `400` with plain-text error output rather than a NetCDF/HDF5 payload. For a more detailed overview of the test cases in this script, see [Test Coverage](#test-coverage).

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

The main design goal was to avoid writing NetCDF payloads to disk. Rocket reads the request body into memory, and the upload handler stores those bytes directly as a `Vec<u8>`. No temporary NetCDF file is created for validation or later merging.

When `/read` is called, the server retrieves the two stored byte vectors and passes them to `combine_netcdf4_in_memory`. `merge.rs` then opens those byte vectors with `nc_open_mem`. NetCDF-C gives each opened in-memory dataset an integer ID, and later calls use those IDs to inspect dimensions, attributes, variables, and data.

The output file is created with `nc_create_mem`, also in memory. After the output dimensions, attributes, variables, and data are written, `nc_close_memio` returns the completed merged file as a C-owned memory buffer. The Rust code copies that final buffer into a Rust-owned `Vec<u8>`, frees the C-owned buffer, and returns the Rust vector through Rocket.

The path is:

```text
HTTP request body
→ Rust Vec<u8> in the upload store
→ nc_open_mem for both inputs
→ nc_create_mem for the output dataset
→ NetCDF-C copy operations
→ nc_close_memio returns the completed output buffer
→ Rust Vec<u8>
→ Rocket response body
```

A confusing detail is that NetCDF-C's memory functions still take a parameter named `path`. In this implementation, those values are names like `memory_part_a`, `memory_part_b`, and `memory_combined`. They are required dataset names for the NetCDF-C API. The server code does not use them as paths for writing the NetCDF payload to disk.

While debugging, I used macOS filesystem tracing tools and saw some library-level behavior from NetCDF-C/HDF5, including dynamic library page-ins and failed path/config probes. I do not treat those as payload disk I/O. The uploaded and merged NetCDF file contents are handled through memory buffers rather than temporary NetCDF files.

## Merge Semantics

A NetCDF dataset has three pieces that matter for this implementation:

1. Dimensions, which name and size axes like `time`, `lat`, or `lon`.
2. Variables, which hold typed data and refer to dimensions.
3. Attributes, which store metadata either globally for the whole file or locally on a specific variable.

This implementation merges each of those pieces directly, one at a time.

It does not try to infer scientific meaning from coordinate variables, align timesteps, or reconcile different grids.

### General merge restrictions

The server only accepts NetCDF-4 and NetCDF-4 classic input files. NetCDF-3, CDF-5, and any other file types are rejected at upload time.

Variables are copied as-is. The implementation supports primitive numeric and char NetCDF types. It does not support strings, compound types, enum types, opaque types, variable-length arrays, groups, or other more complex NetCDF-4 features.

Same-named dimensions must have the same length. Different dimension names can coexist in the output file.

### Dimensions

For each source dimension, the merge reads the dimension's name and length. If the output file does not already have that dimension name, it defines a new output dimension. If the output already has that dimension name with the same length, it reuses the existing output dimension. If the output already has that dimension name with a different length, the merge fails.

This succeeds:

```text
part_a:
  time = 2
  lat = 4
  lon = 3

part_b:
  time = 2
  lat = 4
  lon = 3

combined:
  time = 2
  lat = 4
  lon = 3
```

This fails:

```text
part_a:
  time = 2

part_b:
  time = 5
```

Both files define `time`, but they disagree on its length.

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

Once all variables are defined, the output file leaves define mode. The merge then copies variable data. For each variable, it calculates how many bytes are needed for the full variable, reads the source variable into a temporary byte buffer, and writes that buffer into the corresponding output variable.

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

The next `/read` returns `part_a.nc + overwrite_b.nc`. The integration tests check this by confirming that the first merge contains `temperature` and `humidity`, then re-uploading only `part_b`, then confirming that the next merge contains `temperature` and `pressure` instead.

## Parallelism

The current implementation keeps the request-facing server and the merge path separate. Rocket can still receive independent requests, and the upload store is protected by a Tokio `RwLock`. The part I deliberately serialize is the direct NetCDF-C/HDF5 merge. Before entering that code, `combine_netcdf4_in_memory` acquires a global mutex.

### What is safe to parallelize

The ordinary HTTP work is parallelizable: receiving requests, reading request bodies, validating uploads, storing byte vectors by name, and returning response bytes. Those pieces are normal Rust/Rocket server work.

The merge itself is the sensitive part. NetCDF-4 files are HDF5-backed, and this server enters that stack through C library calls. HDF5's thread-safe build model uses a global lock around entry into the library. The HDF5 multi-threading RFC describes this as allowing only one thread into the library at a time in the thread-safe build. I mirrored that model by using one global merge lock around the NetCDF-C/HDF5 merge path.

### Why NetCDF parallelism is tricky here

NetCDF-C has parallel I/O support, but it is not the same problem as this server's in-memory merge.

For NetCDF-4 files, parallel I/O is built around the HDF5 parallel model. For classic CDF-style files, parallel access goes through PnetCDF, the Parallel-NetCDF library. PnetCDF is built on MPI-IO, where MPI means Message Passing Interface: a standard used by multiple processes in high-performance computing to coordinate work and I/O.

That machinery is useful when a program is already written as a coordinated parallel application, often running across multiple processes. This server is different. It receives two complete uploaded files as HTTP request bodies, opens them from memory, and returns one merged output. The challenge is less about parallel disk reads and more about avoiding unsafe overlap inside the NetCDF-C/HDF5 stack while still allowing the server to handle requests cleanly.

### What I would do next

For a higher-throughput version, I would avoid running multiple NetCDF-C/HDF5 merges concurrently inside one process. I would keep Rocket responsible for HTTP, validation, and upload storage, then move merge work into isolated workers.

A practical version would use a bounded worker process pool. Each worker process would receive one merge job, run one NetCDF-C/HDF5 merge, and return the completed bytes to the Rocket process. In Rust, the process boundary could be built with `std::process::Command` or a small internal worker binary. If I only wanted to move blocking work off the async runtime without process isolation, Tokio's `spawn_blocking` would be relevant, but I would still keep the global NetCDF-C/HDF5 merge lock unless I had very strong evidence that the specific library build and access pattern were safe without it.

I would also add per-name coordination. A production version should make sure that a `GET /read?name=...` sees a consistent pair of bytes and cannot race with a simultaneous overwrite of `part_a` or `part_b` for the same name. A simple first step would be to clone both byte vectors for a name while holding the store lock, release the store lock, then merge that snapshot. For heavier use, I would add per-name locks, request size limits, old-upload cleanup, and worker-process isolation for merge jobs.

Relevant references for this direction include the HDF5 thread-safe library documentation, the HDF5 multi-threading RFC, Tokio's `spawn_blocking` documentation, and Rust's `std::process::Command` documentation.

## Running, Curl Usage, and Testing

These commands assume you are testing locally on port 8000.

### 1. Install Cargo and Python dependencies

Install Rust/Cargo from the official Rust installation page if you do not already have it:

```text
https://www.rust-lang.org/tools/install
```

Install the Python packages used by the fixture and test scripts:

```bash
pip install netCDF4 numpy requests
```

### 2. Start the server

From the project root:

```bash
cargo run
```

The server should be available at:

```text
http://127.0.0.1:8000
```

Keep this terminal running.

### 3. Try the server with curl

In a second terminal, use your own NetCDF-4 files or generated fixtures. Replace the placeholder paths below with the files you want to upload.

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

Inspect it if you have `ncdump` installed:

```bash
ncdump -h <path-to-output-combined.nc>
```

### 4. Generate test data

From the project root:

```bash
python scripts/create_test_data.py
```

This creates `test_data/`, which is ignored by Git because the files are generated.

### 5. Run the integration tests

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

The tests are not exhaustive. They do not currently cover groups, string variables, compound types, variable-length types, compression/chunking preservation, coordinate variables with scientific meaning, or high-concurrency request races. Those are outside the current implementation scope, but they are the first places I would extend testing if this became a production service.

## Limitations

### File format scope

The server accepts NetCDF-4 and NetCDF-4 classic files. NetCDF-3 and CDF-5 are valid NetCDF-family formats, but this server rejects them. The merge path creates a NetCDF-4 output file and is intentionally scoped around NetCDF-4 inputs.

### Structural compatibility

The merge requires same-named dimensions to have the same length. If `part_a` has `time = 2` and `part_b` has `time = 5`, the server does not concatenate, pad, or align the time dimension. It returns an error. Different dimension names can coexist, but the code does not infer that two differently named dimensions might represent the same conceptual axis.

### Scientific meaning

The implementation does not align coordinates, reconcile timesteps, regrid data, concatenate along unlimited dimensions, or detect semantic conflicts between coordinate variables. If both files contain a variable with the same name, the first one copied is kept. That behavior is simple and predictable, but it is not a scientific conflict-resolution strategy.

### NetCDF-4 feature coverage

The implementation supports primitive numeric and char variable data. It does not currently support strings, compound types, enum types, opaque types, variable-length arrays, groups, or other advanced NetCDF-4 structures. It also does not preserve or reason about every possible HDF5-level feature that might exist under a NetCDF-4 file.

### Server lifecycle and memory

Uploaded file pairs are stored in memory without expiration. That is fine for this take-home server, but a longer-running service would need request size limits, cleanup for old names, and a more explicit memory budget.

### Concurrency

The NetCDF-C/HDF5 merge path is serialized. This keeps the implementation safe and easy to reason about, but it means the current version is not designed for high-throughput concurrent merging inside one process.

## Resources and Acknowledgments

### Documentation

* Rocket Programming Guide: `https://rocket.rs/guide`
* Rocket API documentation: `https://docs.rs/rocket`
* Rust installation / Cargo: `https://www.rust-lang.org/tools/install`
* Rust `std::process::Command`: `https://doc.rust-lang.org/std/process/struct.Command.html`
* Tokio `spawn_blocking`: `https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html`
* NetCDF-C file and data I/O documentation: `https://docs.unidata.ucar.edu/netcdf-c/current/group__datasets.html`
* NetCDF-C in-memory support documentation: `https://docs.unidata.ucar.edu/netcdf-c/current/inmemory.html`
* NetCDF-C `netcdf_mem.h` reference: `https://docs.unidata.ucar.edu/netcdf-c/current/netcdf__mem_8h.html`
* NetCDF Users Guide: `https://docs.unidata.ucar.edu/nug/current/`
* HDF5 thread-safe library documentation: `https://support.hdfgroup.org/releases/hdf5/v2_0/v2_0_0/documentation/doxygen/thread-safe-lib.html`
* HDF5 multi-threading RFC: `https://support.hdfgroup.org/releases/hdf5/documentation/rfc/RFC_multi_thread.pdf`
* netCDF4 Python documentation: `https://unidata.github.io/netcdf4-python/`

### YouTube

I used YouTube videos while getting oriented with Rust, Rocket, and NetCDF/HDF5 concepts. Specific videos can be added here.

### Additional tools

I used LLM assistance while working through Rust syntax, Rocket routing, NetCDF-C calls, HDF5 behavior, comments, tests, and README structure. The hardest parts were understanding how NetCDF-C's memory API actually behaves under the hood, separating real payload disk I/O from library/path-probe noise, and making the Rust/C boundary readable enough that I could explain it later.

This was also the fun part of the challenge. I had not worked with Rust or Rocket before, and I had not previously used NetCDF-C's memory API directly. The project forced me to build from examples, docs, filesystem traces, and small test files until the pieces made sense together. I validated the final implementation with generated fixtures, endpoint-level integration tests, `cargo check`, and `cargo test`.
