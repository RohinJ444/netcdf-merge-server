# imports
from netCDF4 import Dataset
import numpy as np
from pathlib import Path

# output directories
out_dir = Path("test_data")
out_dir.mkdir(exist_ok=True)

# part_a.nc: temperature variable + max_temp global attribute
with Dataset(out_dir / "part_a.nc", "w", format="NETCDF4") as ds:
    ds.createDimension("time", 2)
    ds.createDimension("lat", 3)
    ds.createDimension("lon", 4)

    temp = ds.createVariable("temperature", "f4", ("time", "lat", "lon"))
    temp.units = "K"
    temp.long_name = "air temperature"
    temp[:, :, :] = np.arange(2 * 3 * 4, dtype="float32").reshape(2, 3, 4)

    ds.max_temp = 300.0
    ds.title = "Test part A"

# part_b.nc: humidity variable + avg_humidity global attribute
with Dataset(out_dir / "part_b.nc", "w", format="NETCDF4") as ds:
    ds.createDimension("time", 2)
    ds.createDimension("lat", 3)
    ds.createDimension("lon", 4)

    humidity = ds.createVariable("humidity", "f4", ("time", "lat", "lon"))
    humidity.units = "%"
    humidity.long_name = "relative humidity"
    humidity[:, :, :] = np.full((2, 3, 4), 65.0, dtype="float32")

    ds.avg_humidity = 65.0
    ds.title = "Test part B"

print("Wrote and saved test_data/part_a.nc and test_data/part_b.nc")