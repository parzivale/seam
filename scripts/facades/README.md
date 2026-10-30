# Facade generators

`seam-jiff` exposes jiff's whole API except what reaches the system: the
clock (`Timestamp::now`, `Zoned::now`), the system time zone
(`TimeZone::system`, `Zoned: TryFrom<SystemTime>`) and the global tz
database (`TimeZone::get`, `in_tz`, `Zoned: FromStr`, the parsers'
`parse_zoned` and friends). Every type whose API reaches one of those is
wrapped (structs in newtypes, enums mirrored), with every other method and
trait impl forwarded and wrapped types replaced by their wrappers in
every signature; the rest of jiff is re-exported as it is. `seam-jiff`'s
`effectful.rs` writes the left-out functions over effects.

The forwarding is generated from jiff's rustdoc JSON, so it covers every
item and tracks jiff releases:

```sh
scripts/facades/jiff.sh
```

- `rd.py` reads rustdoc JSON; `closure.py` works out which types to wrap
  (those whose API hands out, or takes where no conversion can be added,
  a wrapped type); `jiff.py` writes the facade.
- The effectful functions are listed at the top of `jiff.py`. After a
  jiff upgrade, check its changelog for new ones and list them.

`reqwest.py` (run by `reqwest.sh`) writes seam-reqwest's `ClientBuilder`:
reqwest's, every configuration method forwarded, its `build` giving a
`Reqwest`.
