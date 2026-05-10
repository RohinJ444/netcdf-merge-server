#[macro_use]
extern crate rocket;

// Imports
use rocket::data::{Data, ToByteUnit};
use rocket::http::ContentType;
use rocket::response::status;
use rocket::State;
use std::collections::HashMap;
use tokio::sync::RwLock;
use netcdf_reader::{NcFile, NcFormat};

// Set a maximum file size for each part to prevent crashes from massive files. 
const MAX_UPLOAD_SIZE_GIB: u64 = 1;

// Struct that stores both uploaded NetCDF parts associated with one name
#[derive(Default)]
struct Parts {
    part_a: Option<Vec<u8>>,
    part_b: Option<Vec<u8>>,
}

// HashMap to map each name to a Parts struct of NetCDF files in memory.
// Wrapped in RwLock to allow for concurrent reads and writes. 
// Will later instantiate this such that Rocket manages this as shared application state, 
    // so it persists across separate HTTP requests while the server process is running.
type Store = RwLock<HashMap<String, Parts>>;

// Enables quick checks on server status using the server URL
#[get("/")]
fn index() -> &'static str {
    "Server is running."
}

/// Uploads and stores part A under the name given in a POST request with structure `POST /part_a?name=<name>`
///
/// # Parameters
///
/// - `name`: Query parameter used to identify the corresponding pair of NetCDF files
/// - `data`: Raw request body, which should be a NetCDF-4 file.
/// - `store`: Shared in-memory server state where uploaded files are stored.
///
/// # Returns
///
/// Returns `"stored part_a"` if the body is read, validated as NetCDF-4, and stored successfully. Otherwise, returns `400 Bad Request`.
#[post("/part_a?<name>", data = "<data>")]
async fn upload_part_a(
    name: String,
    data: Data<'_>,
    store: &State<Store>,
) -> Result<&'static str, status::BadRequest<String>> {
    
    // Read uploaded request body into memory
    let bytes = data
        .open(MAX_UPLOAD_SIZE_GIB.gibibytes())
        .into_bytes()
        .await
        .map_err(|e| status::BadRequest(format!("Could not read request body: {e}")))?;

    let bytes = bytes.into_inner();

    // Check that the uploaded bytes are a valid NetCDF-4 file before storing 
    confirm_is_netcdf4(&bytes).map_err(status::BadRequest)?;

    // Get write access to the persisted HashMap
    let mut map = store.write().await;

    // Get the Parts struct for this name, or create an empty one if it does not yet exist 
    let entry = map.entry(name).or_default();

    // Store these bytes as part_a for this name
    entry.part_a = Some(bytes);

    Ok("stored part_a")
}

/// Uploads and stores part B under the name given in a POST request with structure `POST /part_b?name=<name>`
///
/// # Parameters
///
/// - `name`: Query parameter used to identify the corresponding pair of NetCDF files
/// - `data`: Raw request body, which should be a NetCDF-4 file.
/// - `store`: Shared in-memory server state where uploaded files are stored.
///
/// # Returns
///
/// Returns `"stored part_b"` if the body is read, validated as NetCDF-4, and stored successfully. Otherwise, returns `400 Bad Request`.
#[post("/part_b?<name>", data = "<data>")]
async fn upload_part_b(
    name: String,
    data: Data<'_>,
    store: &State<Store>,
) -> Result<&'static str, status::BadRequest<String>> {
    // Read uploaded request body into memory
    let bytes = data
        .open(MAX_UPLOAD_SIZE_GIB.gibibytes())
        .into_bytes()
        .await
        .map_err(|e| status::BadRequest(format!("Could not read request body: {e}")))?;

    let bytes = bytes.into_inner();

    // Check that the uploaded bytes are a valid NetCDF-4 file before storing 
    confirm_is_netcdf4(&bytes).map_err(status::BadRequest)?;

    // Get write access to the persisted HashMap
    let mut map = store.write().await;

    // Get the Parts struct for this name, or create an empty one if it does not yet exist 
    let entry = map.entry(name).or_default();

    // Store these bytes as part_b for this name
    entry.part_b = Some(bytes);

    Ok("stored part_b")
}

/// Checks whether the uploaded bytes can actually be opened as a NetCDF-4 file.
///
/// # Parameters
///
/// - `bytes`: The raw uploaded file bytes read from the HTTP request body.
///
/// # Returns
///
/// Returns `Ok(())` if the bytes can be opened as a NetCDF-4 file, and `Err(String)` with the corresponding error otherwise.
fn confirm_is_netcdf4(bytes: &[u8]) -> Result<(), String> {
    let file = NcFile::from_bytes(bytes)
        .map_err(|e| format!("Could not open uploaded bytes as NetCDF: {e}"))?;

    match file.format() {
        NcFormat::Nc4 | NcFormat::Nc4Classic => Ok(()),
        other => Err(format!("Expected NetCDF-4 file, got {other:?}")),
    }
}


/// Reads both uploaded parts for the name given in a GET request with structure `GET /read?name=<name>`,
///  combines them in memory, and returns the combined bytes.
///
/// # Parameters
///
/// - `name`: Query parameter used to identify the corresponding pair of NetCDF files
/// - `store`: Shared in-memory server state where uploaded files are stored.
///
/// # Returns
///
/// Returns the combined NetCDF bytes if both parts exist and the merge succeeds. 
///  Otherwise, returns `400 Bad Request` with the corresponding error message.
#[get("/read?<name>")]
async fn read_combined(
    name: String,
    store: &State<Store>,
) -> Result<(ContentType, Vec<u8>), status::BadRequest<String>> {
    let map = store.read().await;

    // Check if there is a key in our HashMap store with the given name
    let entry = map
        .get(&name)
        .ok_or_else(|| status::BadRequest(format!("No upload found for name={name}")))?;

    // Check if the retrieved Parts struct has bytes in the part_a field
    let part_a = entry
        .part_a
        .as_ref()
        .ok_or_else(|| status::BadRequest(format!("Part_a is empty for name={name}")))?;

    // Check if the retrieved Parts struct has bytes in the part_b field
    let part_b = entry
        .part_b
        .as_ref()
        .ok_or_else(|| status::BadRequest(format!("Part_b is empty for name={name}")))?;

    // Merge
    let combined = combine_netcdf4_in_memory(part_a, part_b)
        .map_err(|e| status::BadRequest(format!("Could not combine NetCDF files for name={name}. Error: {e}")))?;

    Ok((ContentType::Binary, combined))
}


fn combine_netcdf4_in_memory(a: &[u8], _b: &[u8]) -> Result<Vec<u8>, String> {
    Ok(a.to_vec())
}

#[launch]
fn rocket() -> _ {
    rocket::build()
        .manage(RwLock::new(HashMap::<String, Parts>::new()))
        .mount(
            "/",
            routes![index, upload_part_a, upload_part_b, read_combined],
        )
}