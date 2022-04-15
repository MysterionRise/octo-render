# octo-render
Repository for creating some small subset of OpenGL functionality with the help of these lessons - https://github.com/ssloy/tinyrenderer/wiki/

Later to add raytracing - https://github.com/ssloy/tinyraytracer/wiki

## How to build

    cargo build --release
    ./target/release/octo-render

## How to performance test?

    cargo bench

and check results:

    time:   [3.6928 ms 3.8095 ms 3.9337 ms]
    change: [-15.735% -11.271% -6.3138%] (p = 0.00 < 0.05)
    Performance has improved.
    

### Extra resources

- How OpenGL works: https://github.com/ssloy/tinyrenderer/wiki
