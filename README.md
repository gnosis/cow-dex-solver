DEX COW SOLVER - a demo solver
============================

## Logic of the solver:

- For each order, it requests the best trading route on paraswap and decomposes it into subpath trades
- From the all the subpath trades, it identifies possible cows on the subpath and trades the cows internally against each other.
- Then all the left over volume from the subtrades that don’t fit into a cow is settled against 0x.
- Try to remove all subpath trades form zeroEx with buffer trades
In between, it can fail on many steps, e.g. if the subtrades build a ring, it can’t deal with it.

## How to use it:

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