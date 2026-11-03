# Baked road maps

One `<Town>.json` per CARLA town, drawn under the truck markers on the map.
Bake against a running simulator and commit the result:

```bash
cd demo/carla/sim
./python.sh -m moq_carla_sim.roadmap --out ../web/src/maps
```

The file is a GeoJSON `FeatureCollection` of lane centrelines, but its
coordinates are **CARLA world metres** (x east, y south), not WGS84 degrees:
the same axes the trucks publish on `status.pose`, so the player projects both
with one transform. See `demo/carla/sim/moq_carla_sim/roadmap.py` for why, and
`map.ts` for the projection.

The player matches the file name against the town each truck reports. Nothing
here is required: without a match it fits the view around the trucks themselves
and says so.

## Aerial imagery

Town10HD\_Opt includes a calibrated WebP capture. Its manifest preserves the
capture metadata and image checksum alongside the player's `name`, `image`,
and `bbox` fields. The supplied image already has its horizontal flip applied;
image right is +x and image down is +y. Do not flip it again.

On the machine running CARLA, open a truck camera in the player to activate
rendering, then run:

```bash
cd demo/carla/sim
./python.sh -m moq_carla_sim.aerial
```

The capture reads the loaded town and its baked road bounds. It creates one
temporary RGB camera and removes it even when capture fails; it never ticks,
loads a map, moves trucks, or changes world/render settings. `--carla-host`,
`--carla-port`, `--timeout`, `--resolution`, `--padding`, `--ground-z`, and `--fov`
allow an explicit capture configuration. The frame wait defaults to 30 seconds.

Copy both `<Town>.aerial.png` and `<Town>.aerial.json` into this directory.
The manifest's `image` is a relative basename and `bbox` is
`[minX, minY, maxX, maxY]` in world metres, with +x right and +y down. It also
records `name`, image `width`/`height`, `groundZ`, the camera transform and FOV,
and the simulator `frame`. The PNG and manifest are a pair: ship them together.
WebP is also supported using `<Town>.aerial.webp` and the matching manifest.
Every visitor downloads the image, and the map fits the view to the roads
without zooming further, so ship at most 2560x1440 WebP (Town10HD\_Opt is about
350 KB) and update the manifest's size and checksum to match:

```bash
ffmpeg -i <Town>.aerial.png -vf scale=2560:1440 -c:v libwebp -quality 70 <Town>.aerial.webp
```

Run `just fix` afterwards so the manifests match the repository's formatting.

This is a perspective screenshot calibrated on the `--ground-z` plane, not an
orthophoto. Default narrow FOV limits building lean, but elevated terrain still
shifts. Check the result visually against the baked roads and actual truck
positions before shipping. Rendering or world streaming can leave absent
scenery; a successfully written file does not establish visual alignment.
