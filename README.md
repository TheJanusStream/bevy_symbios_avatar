# bevy_symbios_avatar

Bevy integration for [`symbios-avatar`](https://github.com/TheJanusStream/symbios-avatar),
which builds parametric 3D avatars from atproto records.

This crate draws them. It decides nothing about what a body looks like — every
shape, weight, chart and colour comes from `Avatar::build` — and it holds no
opinion about the scene a body stands in.

## Why it exists

The engine ships with a software renderer, and every quality judgement made
about a body so far has been made through it. Twice that instrument produced a
false diagnosis on its own: a blink that looked broken because lids and globes
were drawn in one colour, and UV seam normals that read as a skinning defect and
sent a day's work after the wrong culprit.

A defect that appears in one renderer and not the other is a defect in the
renderer. One that appears in both is a defect in the body. There is no way to
tell those apart with one instrument.

It is also the only place the shipping tier is real. The budget the engine is
judged against — one to three skinned draws, thirty thousand triangles, a WebGL2
feature set — is a number in a document until something uploads the meshes to a
GPU and a frame comes back.

## Using it

```rust,no_run
use bevy::prelude::*;
use bevy_symbios_avatar::{AvatarPlugin, SpawnAvatar};
use symbios_avatar::{Archetype, AvatarRecord};

App::new()
    .add_plugins((DefaultPlugins, AvatarPlugin))
    .add_systems(Startup, |mut commands: Commands| {
        commands.spawn(SpawnAvatar::from(AvatarRecord::new(
            "Someone",
            Archetype::default(),
        )));
    })
    .run();
```

A body becomes a root entity, one entity per joint of the rig hanging off it in
the rig's own hierarchy, and one entity per drawn mesh — which is also the shape
the draw budget is stated in. Write an `AvatarPose` on the root and the joints
follow.

## The viewer

```text
cargo run --release --example viewer
cargo run --release --example viewer -- --seed 7
cargo run --release --example viewer -- --quadruped
cargo run --release --example viewer -- --shot body.png
```

Drag to turn, scroll to move in, `W` to walk, `Space` to re-roll, `B` to print
what the body costs. Run it in release: building a body subdivides, binds,
unwraps and paints a megapixel atlas.

## The record editor

A panel holding every axis an `AvatarRecord` can carry — about forty of them,
across archetype, skin, eyes, face, hair, outfit, name, seed and the per-category
locks a re-roll honours. Drag one and the body follows.

It is not a debug panel, and two rules are what keep it from becoming one.

**The record, and only the record.** No slider touches an engine constant. A
body tuned against something a record cannot hold could not be saved, shared or
rebuilt by anyone else, and an afternoon spent perfecting one would produce
nothing anybody could keep. `copy` puts the record on the clipboard as JSON and
`load` reads one back, so a body somebody was fiddling with becomes a body
anybody can rebuild. Every value is quantised the way the wire format is —
scaled integers in thousandths, no floats — so what a slider shows is what a
record would hold.

**A judgement image is never a screenshot of a UI.** `H` hides the panel, `P`
hides it for the frame it captures, and `--shot` never opens it at all.

A rebuild costs 68 ms at a draft atlas and 277 at the full one, so an axis
being dragged rebuilds at the draft size — about fourteen frames a second — and
the full-size build lands a quarter of a second after it stops. With the panel
open and nothing moving it costs 0.1 ms a frame.

`default-features = false` removes it, and with it the only dependency this
crate has beyond Bevy and the engine.

## Versions

Bevy 0.18, `bevy_egui` 0.39 — the pair known to work together, rather than
whatever is latest. `symbios-avatar` is a path dependency for as long as the
engine is pre-release: if the two instruments ever read different versions of it
they stop being comparable, which is the one thing this crate is here to do.

## Licence

MIT.
