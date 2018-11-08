## zrs

[![Build status](https://api.travis-ci.org/FauxFaux/zrs.png)](https://travis-ci.org/FauxFaux/zrs)
[![](https://img.shields.io/crates/v/zrs.svg)](https://crates.io/crates/zrs)

`zrs` is a directory switching helper, based on
[rupa's z](https://github.com/rupa/z).

It tracks which directories you frequently visit, and
how recently you have been using them. It will try to take
you to the best matching directory for some inputs.

For example, `z bar` could take you to `/home/you/code/bar`, and
`z foo bar` could take you to `/var/lib/dogfood/libs/bombard`.

## Installation

`zrs` consists of two parts.

 * `zrs` is a Rust binary that needs to
    be in your path. `cargo install zrs` should work, if you have
    `~/.cargo/bin` in your path.

 * `z.sh` is a helper script that must be `source`d in your shell.

`zrs` can add this for you:

```
$ zrs --add-to-profile
written helper script to "/home/faux/.local/share/zrs/z.sh"

couldn't append to "/home/faux/.bashrc": Os { code: 2, kind: NotFound, message: "No such file or directory" }

appended '. .../z.sh' to "/home/faux/.zshrc"
```

## Why?

rupa's shell implementation of `z` has a number of performance and
safety issues. `zrs` solves these by being written as a single binary,
and by being much more careful about touching the filesystem, and
`fork`ing (releasing the shell) before doing anything slow.


## Significant differences

 * some features missing
 * much faster and much less likely to lose your data file writes
    (try holding down return in a shell some time)
 * regex syntax is PCRE
 * missing directories will only be eliminated on explicit `--clean`
