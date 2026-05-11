#![allow(unsafe_op_in_unsafe_fn)]

use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;
use std::sync::{Mutex, OnceLock};

// Global lock used to prevent multiple requests from entering NetCDF-C merge logic at the same time.
// This follows the serialization model described in HDF5's "Thread Safe Library" technical note,
// as otherwise concurrent unsafe merges could overlap inside the underlying C-backed NetCDF/HDF5 state.
static NETCDF_C_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

// Rust version of NetCDF-C's NC_memio struct, which nc_close_memio fills with the final output buffer pointer and size.
#[repr(C)]
struct NcMemio {
    size: usize,
    memory: *mut c_void,
    flags: c_int,
}

// NetCDF-C's in-memory API functions.
unsafe extern "C" {

    // Opens an existing NetCDF file from a caller-provided memory buffer.
    fn nc_open_mem(
        // Although NetCDF-C names this parameter 'path', it is not used here to read
        // from or write to disk; for nc_open_mem, it is a required name for the in-memory dataset.
        path: *const c_char,
        mode: c_int,
        size: usize,
        memory: *mut c_void,
        ncidp: *mut c_int,
    ) -> c_int;

    // Creates a new NetCDF file whose contents are stored in memory.
    fn nc_create_mem(
        // Although NetCDF-C names this parameter 'path', it is not used here to read
        // from or write to disk; for nc_create_mem, it is a required name for the in-memory dataset.
        path: *const c_char,
        mode: c_int,
        initialsize: usize,
        ncidp: *mut c_int,
    ) -> c_int;

    // Closes an in-memory NetCDF file and returns the final memory buffer.
    fn nc_close_memio(ncid: c_int, info: *mut NcMemio) -> c_int;
}

// NetCDF-C string variable API functions.
unsafe extern "C" {
    // Reads an NC_STRING variable into C-allocated string pointers.
    fn nc_get_var_string(
        ncid: c_int,
        varid: c_int,
        data: *mut *mut c_char,
    ) -> c_int;

    // Writes an NC_STRING variable from string pointers.
    fn nc_put_var_string(
        ncid: c_int,
        varid: c_int,
        data: *const *const c_char,
    ) -> c_int;

    // Frees string memory allocated by NetCDF-C.
    fn nc_free_string(
        len: usize,
        data: *mut *mut c_char,
    ) -> c_int;
}

/// Safe public entry point for combining two NetCDF-4 files entirely in memory.
///
/// The rest of the server calls this function instead of the unsafe helper below.
/// This guarantees that every merge first acquires the global merge lock, and that
/// the raw NetCDF-C/HDF5 FFI calls stay isolated in one private function.
///
/// # Parameters
///
/// - `a`: Raw bytes for the first NetCDF-4 file.
/// - `b`: Raw bytes for the second NetCDF-4 file.
///
/// # Returns
///
/// Returns the bytes for the combined NetCDF-4 file if the merge succeeds, and `Err(String)` with the corresponding error message otherwise.
pub fn combine_netcdf4_in_memory(a: &[u8], b: &[u8]) -> Result<Vec<u8>, String> {

    // Acquire the global merge lock so only one NetCDF-C/HDF5 merge runs at a time.
    let _guard = NETCDF_C_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "Could not acquire NetCDF-C lock".to_string())?;

    // All direct C API calls happen inside this unsafe helper
    unsafe { combine_netcdf4_in_memory_unsafe(a, b) }
}

/// Does the actual NetCDF-4 merge using NetCDF-C's in-memory API.
///
/// In this function I perform a structural merge:
/// 1. Copy dimensions from both files
/// 2. Error if same-named dimensions have different lengths
/// 3. Copy global attributes, skipping duplicate names found in part_b
/// 4. Copy variables, skipping duplicate names found in part_b
/// 5. Copy variable attributes and primitive variable data
///
/// This merge checks that same-named dimensions have the same length before copying data.
/// It only supports copying variables as-is. It will not work for cases that require
/// aligning or reconciling data across different grids/timesteps.
///
/// # Parameters
///
/// - `a`: Raw bytes for part A.
/// - `b`: Raw bytes for part B.
///
/// # Returns
///
/// Returns the completed combined NetCDF-4 file as bytes if the merge succeeds, and `Err(String)` otherwise.
unsafe fn combine_netcdf4_in_memory_unsafe(a: &[u8], b: &[u8]) -> Result<Vec<u8>, String> {
    
    // NetCDF-C requires a non-null dataset name for each in-memory file (path parameter described above)
    let name_a = CString::new("memory_part_a").unwrap();
    let name_b = CString::new("memory_part_b").unwrap();
    let name_out = CString::new("memory_combined").unwrap();

    // NetCDF-C refers to open datasets with integer IDs.
    let mut ncid_a: c_int = -1;
    let mut ncid_b: c_int = -1;
    let mut ncid_out: c_int = -1;

    // Open part A from the uploaded memory buffer
    check_nc(
        nc_open_mem(
            name_a.as_ptr(),
            netcdf_sys::NC_NOWRITE,
            a.len(),
            a.as_ptr() as *mut c_void,
            &mut ncid_a,
        ),
        "Could not open part_a from memory",
    )?;

    // Open part B from the uploaded memory buffer
    check_nc(
        nc_open_mem(
            name_b.as_ptr(),
            netcdf_sys::NC_NOWRITE,
            b.len(),
            b.as_ptr() as *mut c_void,
            &mut ncid_b,
        ),
        "Could not open part_b from memory",
    )?;

    // Estimate a starting size for the output memory buffer (can later grow it if needed)
    let initial_size = a.len() + b.len() + 16_384;

    // Create the combined NetCDF-4 output file in memory.
    check_nc(
        nc_create_mem(
            name_out.as_ptr(),
            netcdf_sys::NC_NETCDF4,
            initial_size,
            &mut ncid_out,
        ),
        "Could not create output NetCDF-4 file in memory",
    )?;

    // Map each output dimension name to its length and NetCDF-C dimension ID.
    // This lets dimensions from part_a and part_b share the same output dimension when their names and lengths match.
    let mut out_dims: HashMap<String, (usize, c_int)> = HashMap::new();

    // Copy dimensions from both files and build source-to-output dimension ID maps.
    // These maps are later used when defining variables in the output file.
    let dim_map_a = copy_dimensions(ncid_a, ncid_out, &mut out_dims, "part_a")?;
    let dim_map_b = copy_dimensions(ncid_b, ncid_out, &mut out_dims, "part_b")?;

    // Copy global attributes. If both files have the same global attribute name,
    // keep the first one copied and skip the duplicate.
    let mut copied_global_attrs: HashSet<String> = HashSet::new();
    copy_global_attributes(ncid_a, ncid_out, &mut copied_global_attrs, "part_a")?;
    copy_global_attributes(ncid_b, ncid_out, &mut copied_global_attrs, "part_b")?;

    // Define variables in the output file. If both files have the same variable name,
    // keep the first one copied and skip the duplicate.
    let mut copied_vars: HashSet<String> = HashSet::new();
    let vars_a = define_variables(ncid_a, ncid_out, &dim_map_a, &mut copied_vars, "part_a")?;
    let vars_b = define_variables(ncid_b, ncid_out, &dim_map_b, &mut copied_vars, "part_b")?;

    // End define mode so variable data can be written.
    check_nc(netcdf_sys::nc_enddef(ncid_out), "Could not leave define mode")?;

    // Copy the actual array values for each variable.
    copy_variable_data(ncid_a, ncid_out, &vars_a, "part_a")?;
    copy_variable_data(ncid_b, ncid_out, &vars_b, "part_b")?;

    // Close the two input datasets.
    check_nc(netcdf_sys::nc_close(ncid_a), "Could not close part_a")?;
    check_nc(netcdf_sys::nc_close(ncid_b), "Could not close part_b")?;

    // Initialize the struct that nc_close_memio will fill with the output buffer pointer and size.
    let mut final_mem = NcMemio {
        size: 0,
        memory: ptr::null_mut(),
        flags: 0,
    };

    // Close the in-memory output file and populate final_mem with its bytes.
    check_nc(
        nc_close_memio(ncid_out, &mut final_mem),
        "Could not close output memory NetCDF",
    )?;

    if final_mem.memory.is_null() || final_mem.size == 0 {
        return Err("NetCDF-C returned empty output memory".to_string());
    }

    // Copy the C-owned combined NetCDF output bytes into a Rust Vec<u8> so Rocket can return them safely.
    let output =
        std::slice::from_raw_parts(final_mem.memory as *const u8, final_mem.size).to_vec();

    // Free the C-owned memory allocated by NetCDF-C after copying
    libc::free(final_mem.memory);

    Ok(output)
}

/// Copies dimension definitions from one input file into the output file.
///
/// Dimensions are just metadata, so for each source dimension, this function reads
/// its name and length, then either creates the matching dimension in the output file or reuses
/// an existing output dimension with the same name and length.
///
/// # Parameters
///
/// - `src_ncid`: NetCDF-C ID for the source file.
/// - `dst_ncid`: NetCDF-C ID for the output file.
/// - `out_dims`: Map from output dimension names to their lengths and output IDs.
/// - `label`: Human-readable label used in error messages.
///
/// # Returns
///
/// Returns a vector that maps each source dimension ID to the corresponding output dimension ID.
unsafe fn copy_dimensions(
    src_ncid: c_int,
    dst_ncid: c_int,
    out_dims: &mut HashMap<String, (usize, c_int)>,
    label: &str,
) -> Result<Vec<c_int>, String> {
    let (ndims, _, _, _) = inquire_file(src_ncid, label)?;

    let mut dim_map = vec![-1; ndims as usize];

    // Walk through each source dimension and define or reuse the matching output dimension.
    for dimid in 0..ndims {
        let mut name_buf = vec![0 as c_char; 1024];
        let mut len: usize = 0;

        // Read the dimension's name and length from the source file.
        check_nc(
            netcdf_sys::nc_inq_dim(src_ncid, dimid, name_buf.as_mut_ptr(), &mut len),
            &format!("Could not read dimension {dimid} from {label}"),
        )?;

        let name = cstr_to_string(name_buf.as_ptr())?;

        // If the output already has this dimension name, require the same length.
        if let Some((existing_len, existing_out_id)) = out_dims.get(&name) {
            if *existing_len != len {
                return Err(format!(
                    "Dimension conflict for `{name}`: existing length is {existing_len}, but {label} has length {len}"
                ));
            }

            // Reuse the existing output dimension ID.
            dim_map[dimid as usize] = *existing_out_id;
        // Otherwise, define a new dimension in the output file.
        } else {
            let cname = CString::new(name.clone())
                .map_err(|_| format!("Dimension name contains null byte: {name}"))?;

            let mut out_dimid: c_int = -1;

            check_nc(
                netcdf_sys::nc_def_dim(dst_ncid, cname.as_ptr(), len, &mut out_dimid),
                &format!("Could not define output dimension `{name}`"),
            )?;

            out_dims.insert(name, (len, out_dimid));
            dim_map[dimid as usize] = out_dimid;
        }
    }

    Ok(dim_map)
}

/// Copies global attributes from one input file into the output file.
///
/// # Parameters
///
/// - `src_ncid`: NetCDF-C ID for the source file.
/// - `dst_ncid`: NetCDF-C ID for the output file.
/// - `copied_attrs`: Set of global attribute names already copied.
/// - `label`: Shorthand for input filename; used in error messages.
///
/// # Returns
///
/// Returns `Ok(())` if global attributes are copied successfully.
unsafe fn copy_global_attributes(
    src_ncid: c_int,
    dst_ncid: c_int,
    copied_attrs: &mut HashSet<String>,
    label: &str,
) -> Result<(), String> {
    let (_, _, natts, _) = inquire_file(src_ncid, label)?;

    for attnum in 0..natts {
        let name = get_att_name(src_ncid, netcdf_sys::NC_GLOBAL, attnum)?;

        // Skip duplicate global attributes.
        if copied_attrs.contains(&name) {
            continue;
        }

        let cname = CString::new(name.clone())
            .map_err(|_| format!("Global attribute name contains null byte: {name}"))?;

        // Copy the global attribute directly through NetCDF-C.
        check_nc(
            netcdf_sys::nc_copy_att(
                src_ncid,
                netcdf_sys::NC_GLOBAL,
                cname.as_ptr(),
                dst_ncid,
                netcdf_sys::NC_GLOBAL,
            ),
            &format!("Could not copy global attribute `{name}` from {label}"),
        )?;

        copied_attrs.insert(name);
    }

    Ok(())
}

/// Copies variable definitions from one input file into the output file.
///
/// Variables have to be defined before their data can be written. For each source
/// variable, this function reads its name, type, dimensions, and attributes, then
/// creates the matching variable definition in the output file. The actual variable
/// data is copied later with the copy_variable_data function, which is called after the output file leaves define mode.
///
/// # Parameters
///
/// - `src_ncid`: NetCDF-C ID for the source file.
/// - `dst_ncid`: NetCDF-C ID for the output file.
/// - `dim_map`: Source-to-output dimension ID mapping.
/// - `copied_vars`: Set of variable names already copied.
/// - `label`: Shorthand for input filename; used in error messages.
///
/// # Returns
///
/// Returns a list of `(source_varid, output_varid)` pairs for variables whose data still needs to be copied.
unsafe fn define_variables(
    src_ncid: c_int,
    dst_ncid: c_int,
    dim_map: &[c_int],
    copied_vars: &mut HashSet<String>,
    label: &str,
) -> Result<Vec<(c_int, c_int)>, String> {
    let (_, nvars, _, _) = inquire_file(src_ncid, label)?;

    let mut copied_pairs = Vec::new();

    // Loop through each source variable and define the matching output variable if needed.
    for src_varid in 0..nvars {
        let mut name_buf = vec![0 as c_char; 1024];
        let mut xtype: netcdf_sys::nc_type = 0;
        let mut var_ndims: c_int = 0;
        let mut src_dimids = vec![0 as c_int; 1024];
        let mut var_natts: c_int = 0;

        // Read the variable's name, type, dimensions, and attribute count.
        check_nc(
            netcdf_sys::nc_inq_var(
                src_ncid,
                src_varid,
                name_buf.as_mut_ptr(),
                &mut xtype,
                &mut var_ndims,
                src_dimids.as_mut_ptr(),
                &mut var_natts,
            ),
            &format!("Could not inspect variable {src_varid} from {label}"),
        )?;

        let name = cstr_to_string(name_buf.as_ptr())?;

        // Skip duplicate variable names.
        if copied_vars.contains(&name) {
            continue;
        }

        // Currently limiting my implementation to primitive numeric/char and String types.
        if !is_supported_atomic_type(xtype) {
            return Err(format!(
                "Variable `{name}` from {label} uses unsupported type {xtype}. This implementation supports primitive numeric/char and string NetCDF types only."
            ));
        }

        let mut out_dimids = Vec::new();

        // Convert this variable's source dimension IDs into output dimension IDs.
        for i in 0..var_ndims as usize {
            let src_dimid = src_dimids[i] as usize;

            let out_dimid = *dim_map.get(src_dimid).ok_or_else(|| {
                format!("Variable `{name}` references unknown dimension id {src_dimid}")
            })?;

            out_dimids.push(out_dimid);
        }

        let cname = CString::new(name.clone())
            .map_err(|_| format!("Variable name contains null byte: {name}"))?;

        let mut out_varid: c_int = -1;

        // Define the variable in the output file with the same name, type, and dimensions.
        check_nc(
            netcdf_sys::nc_def_var(
                dst_ncid,
                cname.as_ptr(),
                xtype,
                var_ndims,
                if out_dimids.is_empty() {
                    ptr::null()
                } else {
                    out_dimids.as_ptr()
                },
                &mut out_varid,
            ),
            &format!("Could not define output variable `{name}`"),
        )?;

        // Copy each of the variable's attributes into the output variable.
        for attnum in 0..var_natts {
            let att_name = get_att_name(src_ncid, src_varid, attnum)?;

            let catt_name = CString::new(att_name.clone()).map_err(|_| {
                format!("Variable attribute name contains null byte: {att_name}")
            })?;

            check_nc(
                netcdf_sys::nc_copy_att(
                    src_ncid,
                    src_varid,
                    catt_name.as_ptr(),
                    dst_ncid,
                    out_varid,
                ),
                &format!("Could not copy attribute `{att_name}` for variable `{name}`"),
            )?;
        }

        copied_vars.insert(name);
        copied_pairs.push((src_varid, out_varid));
    }

    Ok(copied_pairs)
}

/// Copies variable data from one input file into the already-defined output variables.
///
/// At this point, with define_variables having already been called for both source files, 
///  the output variables already exist with the right names, types,
/// dimensions, and attributes. This function copies the actual array values by
/// reading each full source variable into a temporary byte buffer and writing that
/// buffer into the corresponding output variable.
///
/// # Parameters
///
/// - `src_ncid`: NetCDF-C ID for the source file.
/// - `dst_ncid`: NetCDF-C ID for the output file.
/// - `var_pairs`: Pairs of source variable IDs and output variable IDs.
/// - `label`: Shorthand for input filename; used in error messages.
///
/// # Returns
///
/// Returns `Ok(())` if all variable data is copied successfully.
unsafe fn copy_variable_data(
    src_ncid: c_int,
    dst_ncid: c_int,
    var_pairs: &[(c_int, c_int)],
    label: &str,
) -> Result<(), String> {

    // Loop through each variable pair and copy the source variable's data into the output variable.
    for (src_varid, dst_varid) in var_pairs {
        let mut name_buf = vec![0 as c_char; 1024];
        let mut xtype: netcdf_sys::nc_type = 0;
        let mut var_ndims: c_int = 0;
        let mut src_dimids = vec![0 as c_int; 1024];
        let mut var_natts: c_int = 0;

        // Read the variable's name, type, and dimensions in order to calculate the size the temporary buffer.
        check_nc(
            netcdf_sys::nc_inq_var(
                src_ncid,
                *src_varid,
                name_buf.as_mut_ptr(),
                &mut xtype,
                &mut var_ndims,
                src_dimids.as_mut_ptr(),
                &mut var_natts,
            ),
            &format!("Could not inspect variable data for varid {src_varid} from {label}"),
        )?;

        let name = cstr_to_string(name_buf.as_ptr())?;

        // Byte size of one element
        let elem_size = nc_type_size(src_ncid, xtype)?;

        // Store a count of how many elements the variable contains.
        let mut elem_count: usize = 1;

        // Multiply each of the dimension lengths together to get the number of elements in the variable.
        for i in 0..var_ndims as usize {
            let mut dim_len: usize = 0;

            check_nc(
                netcdf_sys::nc_inq_dimlen(src_ncid, src_dimids[i], &mut dim_len),
                &format!("Could not inspect dimension length for variable `{name}`"),
            )?;

            elem_count = elem_count
                .checked_mul(dim_len)
                .ok_or_else(|| format!("Variable `{name}` is too large"))?;
        }

        // NC_STRING data is copied through NetCDF-C's string-specific API because
        // each element is a C string pointer, as opposed to fixed-width inline data.
        if xtype == netcdf_sys::NC_STRING {
            let mut strings: Vec<*mut c_char> = vec![ptr::null_mut(); elem_count];

            check_nc(
                nc_get_var_string(src_ncid, *src_varid, strings.as_mut_ptr()),
                &format!("Could not read string data for variable `{name}` from {label}"),
            )?;

            let put_status = nc_put_var_string(
                dst_ncid,
                *dst_varid,
                strings.as_ptr() as *const *const c_char,
            );

            let free_status = nc_free_string(elem_count, strings.as_mut_ptr());

            check_nc(
                put_status,
                &format!("Could not write string data for variable `{name}` to output"),
            )?;

            check_nc(
                free_status,
                &format!("Could not free string data for variable `{name}`"),
            )?;

            continue;
        }

        // Convert the element count into a byte count, checking for overflow
        let total_bytes = elem_count
            .checked_mul(elem_size)
            .ok_or_else(|| format!("Variable `{name}` is too large"))?;

        let mut buffer = vec![0u8; total_bytes];

        // Read the source variable's bytes into the temporary buffer.
        check_nc(
            netcdf_sys::nc_get_var(src_ncid, *src_varid, buffer.as_mut_ptr() as *mut c_void),
            &format!("Could not read data for variable `{name}` from {label}"),
        )?;

        // Write those bytes into the corresponding output variable.
        check_nc(
            netcdf_sys::nc_put_var(dst_ncid, *dst_varid, buffer.as_ptr() as *const c_void),
            &format!("Could not write data for variable `{name}` to output"),
        )?;
    }

    Ok(())
}

/// Reads the basic structural counts for a NetCDF-C dataset.
///
/// Several merge steps need to know how many dimensions, variables, or global
/// attributes a file contains. This helper retrieves those counts through
/// NetCDF-C's `nc_inq` function and returns them in one place.
///
/// # Parameters
///
/// - `ncid`: NetCDF-C ID for the file.
/// - `label`: Shorthand for input filename; used in error messages.
///
/// # Returns
///
/// Returns `(ndims, nvars, natts, unlimdimid)`, where:
/// - `ndims`: Number of dimensions defined in the file.
/// - `nvars`: Number of variables defined in the file.
/// - `natts`: Number of global attributes defined in the file.
/// - `unlimdimid`: NetCDF-C ID of the unlimited dimension, or `-1` if there is none.
unsafe fn inquire_file(ncid: c_int, label: &str) -> Result<(c_int, c_int, c_int, c_int), String> {
    let mut ndims: c_int = 0;
    let mut nvars: c_int = 0;
    let mut natts: c_int = 0;
    let mut unlimdimid: c_int = -1;

    check_nc(
        netcdf_sys::nc_inq(
            ncid,
            &mut ndims,
            &mut nvars,
            &mut natts,
            &mut unlimdimid,
        ),
        &format!("Could not inspect file structure for {label}"),
    )?;

    Ok((ndims, nvars, natts, unlimdimid))
}

/// Gets the name of an attribute from a NetCDF-C dataset.
///
/// NetCDF-C identifies attributes by index when iterating through them. This helper
/// asks NetCDF-C for the attribute name at that index and converts the returned
/// C string into a Rust `String`, so the merge logic can compare, store, and reuse attribute names safely.
///
/// # Parameters
///
/// - `ncid`: NetCDF-C ID for the file.
/// - `varid`: Variable ID, or `NC_GLOBAL` for a global attribute.
/// - `attnum`: Attribute index.
///
/// # Returns
///
/// Returns the attribute name as a Rust `String`.
unsafe fn get_att_name(ncid: c_int, varid: c_int, attnum: c_int) -> Result<String, String> {
    let mut name_buf = vec![0 as c_char; 1024];

    check_nc(
        netcdf_sys::nc_inq_attname(ncid, varid, attnum, name_buf.as_mut_ptr()),
        &format!("Could not read attribute name {attnum}"),
    )?;

    cstr_to_string(name_buf.as_ptr())
}

/// Gets the byte size of a NetCDF type.
///
/// When copying variable data through a raw byte buffer, it is necessary to know how many
/// bytes each element occupies. This helper asks NetCDF-C for the size of the
/// variable's type, such as 4 bytes for `NC_FLOAT` or 8 bytes for `NC_DOUBLE`.
///
/// # Parameters
///
/// - `ncid`: NetCDF-C ID for the file.
/// - `xtype`: NetCDF-C type ID.
///
/// # Returns
///
/// Returns the number of bytes for one element of this type.
unsafe fn nc_type_size(ncid: c_int, xtype: netcdf_sys::nc_type) -> Result<usize, String> {
    let mut size: usize = 0;

    check_nc(
        netcdf_sys::nc_inq_type(ncid, xtype, ptr::null_mut(), &mut size),
        &format!("Could not get size for NetCDF type {xtype}"),
    )?;

    Ok(size)
}

/// Checks whether a NetCDF type is supported by this merge implementation.
///
/// This implementation supports fixed-width primitive numeric/char types and
/// NetCDF string variables. Other NetCDF-4 types require additional type-specific
/// schema or memory handling.
///
/// # Parameters
///
/// - `xtype`: NetCDF-C type ID.
///
/// # Returns
///
/// Returns `true` for supported primitive numeric/char and string types.
fn is_supported_atomic_type(xtype: netcdf_sys::nc_type) -> bool {
    xtype == netcdf_sys::NC_BYTE
        || xtype == netcdf_sys::NC_CHAR
        || xtype == netcdf_sys::NC_SHORT
        || xtype == netcdf_sys::NC_INT
        || xtype == netcdf_sys::NC_FLOAT
        || xtype == netcdf_sys::NC_DOUBLE
        || xtype == netcdf_sys::NC_UBYTE
        || xtype == netcdf_sys::NC_USHORT
        || xtype == netcdf_sys::NC_UINT
        || xtype == netcdf_sys::NC_INT64
        || xtype == netcdf_sys::NC_UINT64
        || xtype == netcdf_sys::NC_STRING
}

/// Converts a C string pointer into a Rust `String`.
///
/// # Parameters
///
/// - `ptr`: Pointer to a null-terminated C string.
///
/// # Returns
///
/// Returns the corresponding Rust `String`, or an error if the string is not valid UTF-8.
unsafe fn cstr_to_string(ptr: *const c_char) -> Result<String, String> {
    CStr::from_ptr(ptr)
        .to_str()
        .map(|s| s.to_string())
        .map_err(|e| format!("Invalid UTF-8 from NetCDF name: {e}"))
}

/// Converts a NetCDF-C status code into a Rust `Result`.
///
/// # Parameters
///
/// - `status`: Status code returned by a NetCDF-C function.
/// - `context`: Message describing what we were trying to do.
///
/// # Returns
///
/// Returns `Ok(())` if NetCDF-C returned `NC_NOERR`, and `Err(String)` with the NetCDF-C error message otherwise.
fn check_nc(status: c_int, context: &str) -> Result<(), String> {
    if status == netcdf_sys::NC_NOERR {
        Ok(())
    } else {
        unsafe {
            let msg = CStr::from_ptr(netcdf_sys::nc_strerror(status))
                .to_string_lossy()
                .into_owned();

            Err(format!("{context}: {msg}"))
        }
    }
}