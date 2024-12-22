run: 
    mprocs

build: 
    cargo build
    just uno
uno:
    ./node_modules/@unocss/cli/bin/unocss.mjs "templates/**/*.html" -o "static/uno.css"
