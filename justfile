run: 
    just init
    RUST_LOG=info mprocs

init:
    mkdir -p ./dev-data
    sqlx db setup
