# imports
from pathlib import Path

import numpy as np
from netCDF4 import Dataset

# configure output directory
out_dir = Path("test_data")
out_dir.mkdir(exist_ok=True)

# make random test data deterministic
rng = np.random.default_rng(seed=42)

# Creates a basic, compatible merge case with small files
def write_standard_pair():
    with Dataset(out_dir / "part_a.nc", "w", format="NETCDF4") as ds:
        ds.createDimension("time", 2)
        ds.createDimension("lat", 4)
        ds.createDimension("lon", 3)

        temp = ds.createVariable("temperature", "f4", ("time", "lat", "lon"))
        temp.units = "K"
        temp.long_name = "air temperature"
        temp[:, :, :] = rng.random((2, 4, 3), dtype=np.float32) * 40 + 260

        ds.max_temp = 300.0
        ds.title = "Test part A"

    with Dataset(out_dir / "part_b.nc", "w", format="NETCDF4") as ds:
        ds.createDimension("time", 2)
        ds.createDimension("lat", 4)
        ds.createDimension("lon", 3)

        humidity = ds.createVariable("humidity", "f4", ("time", "lat", "lon"))
        humidity.units = "%"
        humidity.long_name = "relative humidity"
        humidity[:, :, :] = rng.random((2, 4, 3), dtype=np.float32) * 100

        ds.avg_humidity = 65.0
        ds.title = "Test part B"

# Creates an alternate part_b file used to test overwriting an existing upload
def write_overwrite_part_b():
    with Dataset(out_dir / "overwrite_b.nc", "w", format="NETCDF4") as ds:
        ds.createDimension("time", 2)
        ds.createDimension("lat", 4)
        ds.createDimension("lon", 3)

        pressure = ds.createVariable("pressure", "f4", ("time", "lat", "lon"))
        pressure.units = "hPa"
        pressure.long_name = "air pressure"
        pressure[:, :, :] = np.full((2, 4, 3), 1013.25, dtype=np.float32)

        ds.avg_pressure = 1013.25
        ds.title = "Overwrite part B"

# Creates files that should fail when merging because same-named dimensions have different lengths
def write_dimension_conflict_pair():
    with Dataset(out_dir / "dim_conflict_a.nc", "w", format="NETCDF4") as ds:
        ds.createDimension("time", 2)
        ds.createDimension("lat", 4)
        ds.createDimension("lon", 3)

        temp = ds.createVariable("temperature", "f4", ("time", "lat", "lon"))
        temp[:, :, :] = rng.random((2, 4, 3), dtype=np.float32)

    with Dataset(out_dir / "dim_conflict_b.nc", "w", format="NETCDF4") as ds:
        ds.createDimension("time", 5)
        ds.createDimension("lat", 4)
        ds.createDimension("lon", 3)

        humidity = ds.createVariable("humidity", "f4", ("time", "lat", "lon"))
        humidity[:, :, :] = rng.random((5, 4, 3), dtype=np.float32)


# Creates files where both inputs contain the same variable name
def write_duplicate_variable_pair():
    with Dataset(out_dir / "duplicate_var_a.nc", "w", format="NETCDF4") as ds:
        ds.createDimension("time", 2)

        temp = ds.createVariable("temperature", "f4", ("time",))
        temp.units = "K"
        temp[:] = np.array([280.0, 281.0], dtype=np.float32)

        ds.source = "part_a"

    with Dataset(out_dir / "duplicate_var_b.nc", "w", format="NETCDF4") as ds:
        ds.createDimension("time", 2)

        temp = ds.createVariable("temperature", "f4", ("time",))
        temp.units = "C"
        temp[:] = np.array([10.0, 11.0], dtype=np.float32)

        ds.source_b = "part_b"


# Creates files with different dimension names that should both be preserved
def write_disjoint_dimension_pair():
    with Dataset(out_dir / "disjoint_dims_a.nc", "w", format="NETCDF4") as ds:
        ds.createDimension("station", 3)

        temp = ds.createVariable("station_temperature", "f4", ("station",))
        temp[:] = np.array([280.0, 281.0, 282.0], dtype=np.float32)

    with Dataset(out_dir / "disjoint_dims_b.nc", "w", format="NETCDF4") as ds:
        ds.createDimension("height", 2)

        wind = ds.createVariable("wind_speed", "f4", ("height",))
        wind[:] = np.array([5.0, 7.0], dtype=np.float32)


# Creates a larger compatible merge case
def write_large_pair():
    TIME = 50
    LAT = 500
    LON = 500

    with Dataset(out_dir / "large_a.nc", "w", format="NETCDF4") as ds:
        ds.createDimension("time", TIME)
        ds.createDimension("lat", LAT)
        ds.createDimension("lon", LON)

        temp = ds.createVariable("temperature", "f4", ("time", "lat", "lon"))
        temp.units = "K"
        temp[:, :, :] = rng.random((TIME, LAT, LON), dtype=np.float32) * 40 + 260

        ds.max_temp = 300.0

    with Dataset(out_dir / "large_b.nc", "w", format="NETCDF4") as ds:
        ds.createDimension("time", TIME)
        ds.createDimension("lat", LAT)
        ds.createDimension("lon", LON)

        humidity = ds.createVariable("humidity", "f4", ("time", "lat", "lon"))
        humidity.units = "%"
        humidity[:, :, :] = rng.random((TIME, LAT, LON), dtype=np.float32) * 100

        ds.avg_humidity = 65.0

# Creates a non-NetCDF file for upload rejection testing
def write_invalid_file():
    (out_dir / "not_netcdf.txt").write_text("this is not a NetCDF file\n")

# Creates a NetCDF-3 file that should be rejected since the server only accepts NetCDF-4 files
def write_netcdf3_file():
    with Dataset(out_dir / "netcdf3.nc", "w", format="NETCDF3_CLASSIC") as ds:
        ds.createDimension("time", 2)

        temp = ds.createVariable("temperature", "f4", ("time",))
        temp[:] = np.array([280.0, 281.0], dtype=np.float32)

# Creates a CDF-5 file that should be rejected because the server only accepts NetCDF-4 files
def write_cdf5_file():
    with Dataset(out_dir / "cdf5.nc", "w", format="NETCDF3_64BIT_DATA") as ds:
        ds.createDimension("time", 2)

        temp = ds.createVariable("temperature", "f4", ("time",))
        temp[:] = np.array([280.0, 281.0], dtype=np.float32)

# Creates NetCDF-4 files with NC_STRING variables
def write_string_variable_pair():
    with Dataset(out_dir / "string_a.nc", "w", format="NETCDF4") as ds:
        ds.createDimension("station", 3)

        names = ds.createVariable("station_name", str, ("station",))
        names.long_name = "station name"
        names[:] = np.array(["oakland", "berkeley", "richmond"], dtype=object)

        ds.string_source = "part_a"

    with Dataset(out_dir / "string_b.nc", "w", format="NETCDF4") as ds:
        ds.createDimension("station", 3)

        labels = ds.createVariable("station_label", str, ("station",))
        labels.long_name = "station label"
        labels[:] = np.array(["OAK", "BERK", "RICH"], dtype=object)

        ds.string_source_b = "part_b"

write_standard_pair()
write_overwrite_part_b()
write_dimension_conflict_pair()
write_duplicate_variable_pair()
write_disjoint_dimension_pair()
write_large_pair()
write_invalid_file()
write_netcdf3_file()
write_cdf5_file()
write_string_variable_pair()

print(f"Wrote NetCDF test data to {out_dir}/")