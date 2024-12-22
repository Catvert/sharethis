run: 
    just init
    RUST_LOG=info mprocs

init:
    mkdir -p ./data
    sqlx db create
    sqlx migrate run

build: 
    cargo build
    just uno
uno:
    ./node_modules/@unocss/cli/bin/unocss.mjs "templates/**/*.html" -o "static/uno.css"
