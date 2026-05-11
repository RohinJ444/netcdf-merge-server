# imports
from pathlib import Path

import requests
from netCDF4 import Dataset

BASE_URL = "http://127.0.0.1:8000"
TEST_DIR = Path("test_data")

# # # # # #
# HELPERS #
# # # # # #

# Uploads one file to either /part_a or /part_b.
def post_file(endpoint: str, name: str, path: Path) -> requests.Response:
    with path.open("rb") as f:
        return requests.post(
            f"{BASE_URL}/{endpoint}",
            params={"name": name},
            data=f.read(),
            timeout=10,
        )


# Requests the combined NetCDF file for a stored name.
def read_combined(name: str) -> requests.Response:
    return requests.get(f"{BASE_URL}/read", params={"name": name}, timeout=10)

# Uploads a pair of NetCDF files under the same merge name.
def upload_pair(name: str, part_a: str, part_b: str) -> None:
    resp_a = post_file("part_a", name, TEST_DIR / part_a)
    assert resp_a.status_code == 200, f"part_a upload failed: {resp_a.status_code} {resp_a.text}"

    resp_b = post_file("part_b", name, TEST_DIR / part_b)
    assert resp_b.status_code == 200, f"part_b upload failed: {resp_b.status_code} {resp_b.text}"


# Saves a successful /read response as a local NetCDF file for inspection.
def save_response_bytes(resp: requests.Response, filename: str) -> Path:
    out_path = TEST_DIR / filename
    out_path.write_bytes(resp.content)
    return out_path


# Checks that a failed request returned plain-text error output, not a NetCDF file.
def assert_error_response(resp: requests.Response, expected_text: str) -> None:
    assert resp.status_code == 400
    assert expected_text in resp.text

    # NetCDF-4 files are HDF5 files, which start with this eight-byte signature.
    assert not resp.content.startswith(b"\x89HDF\r\n\x1a\n")

# # # # # 
# TESTS #
# # # # # 

# Confirms the Rocket server is running
def test_server_is_running() -> None:
    resp = requests.get(BASE_URL, timeout=5)
    assert resp.status_code == 200
    assert "Server is running" in resp.text


# Tests a basic, compatible merge path: two files with the same dimensions, different variables and global attributes.
def test_standard_merge() -> None:
    upload_pair("standard", "part_a.nc", "part_b.nc")

    resp = read_combined("standard")
    assert resp.status_code == 200, resp.text

    out_path = save_response_bytes(resp, "combined_standard.nc")

    with Dataset(out_path, "r") as ds:
        assert set(ds.dimensions.keys()) == {"time", "lat", "lon"}
        assert len(ds.dimensions["time"]) == 2
        assert len(ds.dimensions["lat"]) == 4
        assert len(ds.dimensions["lon"]) == 3

        assert set(ds.variables.keys()) == {"temperature", "humidity"}

        temperature = ds.variables["temperature"]
        humidity = ds.variables["humidity"]

        assert temperature.shape == (2, 4, 3)
        assert humidity.shape == (2, 4, 3)

        assert temperature.units == "K"
        assert temperature.long_name == "air temperature"

        assert humidity.units == "%"
        assert humidity.long_name == "relative humidity"

        assert ds.max_temp == 300.0
        assert ds.avg_humidity == 65.0

        # Duplicate global attribute should keep part_a because part_a is copied first.
        assert ds.title == "Test part A"

# Tests that re-uploading part_b for an existing name replaces the previous part_b.
def test_reupload_part_b_overwrites_existing_merge() -> None:
    name = "overwrite_existing"

    # Read in the original combined output before doing any overwrite.
    upload_pair(name, "part_a.nc", "part_b.nc")

    first_resp = read_combined(name)
    assert first_resp.status_code == 200, first_resp.text

    first_out_path = save_response_bytes(first_resp, "combined_before_overwrite.nc")

    # Check that the first combined file was read in correctly
    with Dataset(first_out_path, "r") as ds:
        assert set(ds.dimensions.keys()) == {"time", "lat", "lon"}
        assert len(ds.dimensions["time"]) == 2
        assert len(ds.dimensions["lat"]) == 4
        assert len(ds.dimensions["lon"]) == 3

        assert set(ds.variables.keys()) == {"temperature", "humidity"}

        temperature = ds.variables["temperature"]
        humidity = ds.variables["humidity"]

        assert temperature.shape == (2, 4, 3)
        assert humidity.shape == (2, 4, 3)

        assert temperature.units == "K"
        assert temperature.long_name == "air temperature"

        assert humidity.units == "%"
        assert humidity.long_name == "relative humidity"

        assert ds.max_temp == 300.0
        assert ds.avg_humidity == 65.0
        assert ds.title == "Test part A"

    # Re-upload only part_b under the same name. This should overwrite the old part_b bytes but leave part_a unchanged:
    resp_b = post_file("part_b", name, TEST_DIR / "overwrite_b.nc")
    assert resp_b.status_code == 200, resp_b.text

    # Read in the combined output after the overwrite.
    second_resp = read_combined(name)
    assert second_resp.status_code == 200, second_resp.text

    second_out_path = save_response_bytes(second_resp, "combined_after_overwrite.nc")

    # Check that the second combined file reflects the updated pair. It should still contain temperature from part_a, but humidity should now be gone
    # and pressure from overwrite_b.nc should now be present.
    with Dataset(second_out_path, "r") as ds:
        assert set(ds.dimensions.keys()) == {"time", "lat", "lon"}
        assert len(ds.dimensions["time"]) == 2
        assert len(ds.dimensions["lat"]) == 4
        assert len(ds.dimensions["lon"]) == 3

        assert set(ds.variables.keys()) == {"temperature", "pressure"}

        temperature = ds.variables["temperature"]
        pressure = ds.variables["pressure"]

        assert temperature.shape == (2, 4, 3)
        assert pressure.shape == (2, 4, 3)

        assert temperature.units == "K"
        assert temperature.long_name == "air temperature"

        assert pressure.units == "hPa"
        assert pressure.long_name == "air pressure"

        assert ds.max_temp == 300.0
        assert ds.avg_pressure == 1013.25
        assert ds.title == "Test part A"

        assert not hasattr(ds, "avg_humidity")
        assert "humidity" not in ds.variables

        assert float(pressure[0, 0, 0]) == 1013.25
        assert float(pressure[1, 3, 2]) == 1013.25


# Tests that same-named dimensions with different lengths return a clear 400 error
def test_dimension_conflict_returns_400() -> None:
    upload_pair("dimension_conflict", "dim_conflict_a.nc", "dim_conflict_b.nc")

    resp = read_combined("dimension_conflict")
    assert_error_response(resp, "Dimension conflict")
    assert "time" in resp.text


# Tests that duplicate variable names are handled by keeping the first copied variable
def test_duplicate_variable_keeps_first() -> None:
    upload_pair("duplicate_variable", "duplicate_var_a.nc", "duplicate_var_b.nc")

    resp = read_combined("duplicate_variable")
    assert resp.status_code == 200, resp.text

    out_path = save_response_bytes(resp, "combined_duplicate_variable.nc")

    with Dataset(out_path, "r") as ds:
        assert set(ds.dimensions.keys()) == {"time"}
        assert len(ds.dimensions["time"]) == 2

        assert set(ds.variables.keys()) == {"temperature"}

        temperature = ds.variables["temperature"]
        assert temperature.shape == (2,)
        assert temperature.units == "K"

        # part_a values should win because part_a variables are copied first
        assert float(temperature[0]) == 280.0
        assert float(temperature[1]) == 281.0

        # part_b-only global attr should still be copied
        assert ds.source == "part_a"
        assert ds.source_b == "part_b"


# Tests that files with disjoint dimension names can still be structurally merged
def test_disjoint_dimensions_merge() -> None:
    upload_pair("disjoint_dimensions", "disjoint_dims_a.nc", "disjoint_dims_b.nc")

    resp = read_combined("disjoint_dimensions")
    assert resp.status_code == 200, resp.text

    out_path = save_response_bytes(resp, "combined_disjoint_dimensions.nc")

    with Dataset(out_path, "r") as ds:
        assert set(ds.dimensions.keys()) == {"station", "height"}
        assert len(ds.dimensions["station"]) == 3
        assert len(ds.dimensions["height"]) == 2

        assert set(ds.variables.keys()) == {"station_temperature", "wind_speed"}

        station_temperature = ds.variables["station_temperature"]
        wind_speed = ds.variables["wind_speed"]

        assert station_temperature.shape == (3,)
        assert wind_speed.shape == (2,)

        assert float(station_temperature[0]) == 280.0
        assert float(station_temperature[1]) == 281.0
        assert float(station_temperature[2]) == 282.0

        assert float(wind_speed[0]) == 5.0
        assert float(wind_speed[1]) == 7.0


# Tests the error path where part_b has not been uploaded yet
def test_missing_part_b_returns_400() -> None:
    name = "missing_part_b"

    resp_a = post_file("part_a", name, TEST_DIR / "part_a.nc")
    assert resp_a.status_code == 200, resp_a.text

    resp = read_combined(name)
    assert_error_response(resp, "Part_b is empty")


# Tests the error path where part_a has not been uploaded yet
def test_missing_part_a_returns_400() -> None:
    name = "missing_part_a"

    resp_b = post_file("part_b", name, TEST_DIR / "part_b.nc")
    assert resp_b.status_code == 200, resp_b.text

    resp = read_combined(name)
    assert_error_response(resp, "Part_a is empty")


# Tests that files that aren't NetCDF-4 files uploads are rejected before storage
def test_invalid_upload_returns_400() -> None:
    resp = post_file("part_a", "invalid_upload", TEST_DIR / "not_netcdf.txt")
    assert resp.status_code == 400
    assert "NetCDF" in resp.text or "HDF5" in resp.text
    assert not resp.content.startswith(b"\x89HDF\r\n\x1a\n")

# Tests that NetCDF-3 files are rejected because the server expects NetCDF-4/HDF5.
def test_netcdf3_upload_returns_400() -> None:
    resp = post_file("part_a", "netcdf3_upload", TEST_DIR / "netcdf3.nc")
    assert_error_response(resp, "NetCDF-4")

# Tests that valid CDF-5 files are rejected because the server expects NetCDF-4/HDF5.
def test_cdf5_upload_returns_400() -> None:
    resp = post_file("part_a", "cdf5_upload", TEST_DIR / "cdf5.nc")
    assert_error_response(resp, "NetCDF-4")

# Tests the same compatible merge (two files with the same dimensions, different variables and global attributes) but with larger files
def test_large_merge() -> None:
    upload_pair("large", "large_a.nc", "large_b.nc")

    resp = read_combined("large")
    assert resp.status_code == 200, resp.text

    out_path = save_response_bytes(resp, "combined_large.nc")

    with Dataset(out_path, "r") as ds:
        assert set(ds.dimensions.keys()) == {"time", "lat", "lon"}
        assert len(ds.dimensions["time"]) == 50
        assert len(ds.dimensions["lat"]) == 500
        assert len(ds.dimensions["lon"]) == 500

        assert set(ds.variables.keys()) == {"temperature", "humidity"}

        temperature = ds.variables["temperature"]
        humidity = ds.variables["humidity"]

        assert temperature.shape == (50, 500, 500)
        assert humidity.shape == (50, 500, 500)

        assert temperature.units == "K"
        assert humidity.units == "%"

        assert ds.max_temp == 300.0
        assert ds.avg_humidity == 65.0

# Tests that NC_STRING variables are copied correctly.
def test_string_variables_merge() -> None:
    upload_pair("string_variables", "string_a.nc", "string_b.nc")

    resp = read_combined("string_variables")
    assert resp.status_code == 200, resp.text

    out_path = save_response_bytes(resp, "combined_string_variables.nc")

    with Dataset(out_path, "r") as ds:
        assert set(ds.dimensions.keys()) == {"station"}
        assert len(ds.dimensions["station"]) == 3

        assert set(ds.variables.keys()) == {"station_name", "station_label"}

        station_name = ds.variables["station_name"]
        station_label = ds.variables["station_label"]

        assert station_name.shape == (3,)
        assert station_label.shape == (3,)

        assert station_name.long_name == "station name"
        assert station_label.long_name == "station label"

        assert list(station_name[:]) == ["oakland", "berkeley", "richmond"]
        assert list(station_label[:]) == ["OAK", "BERK", "RICH"]

        assert ds.string_source == "part_a"
        assert ds.string_source_b == "part_b"

def main() -> None:
    tests = [
        test_server_is_running,
        test_standard_merge,
        test_reupload_part_b_overwrites_existing_merge,
        test_dimension_conflict_returns_400,
        test_duplicate_variable_keeps_first,
        test_disjoint_dimensions_merge,
        test_missing_part_b_returns_400,
        test_missing_part_a_returns_400,
        test_invalid_upload_returns_400,
        test_netcdf3_upload_returns_400,
        test_cdf5_upload_returns_400,
        test_large_merge,
        test_string_variables_merge,
    ]

    for test in tests:
        test()
        print(f"PASS: {test.__name__}")

    print("All integration tests passed.")

if __name__ == "__main__":
    main()