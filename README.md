DEX COW SOLVER - a demo solver
============================

## Logic of the solver:

- For each order, it requests the best trading route on paraswap and decomposes it into subpath trades
- From the all the subpath trades, it identifies possible cows on the subpath and trades the cows internally against each other.
- Then all the left over volume from the subtrades that don’t fit into a cow is settled against 0x.
- Try to remove all subpath trades from zeroEx with buffer trades
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

## How to run simulations together with the cowswap official driver:

In order to see how the solutions would be submitted, which exact settlements would be build with the provided solutions and in order to investigate the settlements-simulations in tenderly, you can follow the following steps:

### Prepare driver code:

Run the following commands to get the cowswap driver repo
```
git clone git@github.com:gnosis/gp-v2-services.git
cd gp-v2-services
```
and then start the driver by:
```
export INFURA_KEY=<your infura key>

cargo run -p solver --  --orderbook-url https://protocol-mainnet.gnosis.io \
   --node-url "https://mainnet.infura.io/v3/$INFURA_KEY" \
  -—cow-dex-ag-solver-url "http://127.0.0.1:8000" \
  --solver-account 0xa6DDBD0dE6B310819b49f680F65871beE85f517e \
  --solvers CowDexAg \
  --transaction-strategy DryRun
```

This will start the driver.

Then you want to start the cowDexAg solver by running from this repo:
```
cargo run
```

Now, the driver should supply current open orders from production to the cowDexAg solver and the solver will find a solution and returning it back to the driver.
