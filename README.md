# Gamevim

Gamevim is a terminal game for learning Vim habits through short, replayable lessons.

Run the app with:

```sh
cargo run
```

Useful quick commands:

```sh
cargo run -- --list-levels
cargo run -- --lesson 1-2
cargo run -- --lesson 1-2 --seed 42
cargo run -- --reset-progress
```

The default `gamevim` experience opens a full terminal UI with Continue, Lessons, Help, Practice, and Settings screens. In menus you can use arrow keys or `h/j/k/l`; inside levels, only Vim commands taught so far are accepted.
