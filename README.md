DEX COW SOLVER - a demo solver
============================

Start the server by running:
```
cargo run
```

then post requests to it like:
```
curl -vX POST "http://127.0.0.1:8000/solve" -H  "accept: application/json" -H  "Content-Type: application/json" --data "@/Users/alexherrmann/gnosis/gp-v2-solver-lib/data/test.json"
```

Alternatively, the code can also be run via docker:

Running api
```
docker build -t gpdata -f docker/Dockerfile.binary . 
docker run -ti cowdexsolver cowdexsolver    
```