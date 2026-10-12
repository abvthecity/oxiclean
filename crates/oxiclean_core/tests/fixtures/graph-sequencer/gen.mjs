// Generates cases.json: random graphs with the output of pnpm's
// graph-sequencer, which the Rust port in src/sequencer.rs must reproduce.
//
//   npm pack @pnpm/deps.graph-sequencer@1101.0.0 && tar xzf pnpm-deps.graph-sequencer-1101.0.0.tgz
//   node gen.mjs package/lib/index.js > cases.json
const { graphSequencer } = await import(new URL(process.argv[2], `file://${process.cwd()}/`));
// Deterministic PRNG (mulberry32)
let seed = 0x5eed;
const rand = () => { seed |= 0; seed = seed + 0x6D2B79F5 | 0; let t = Math.imul(seed ^ seed >>> 15, 1 | seed); t = t + Math.imul(t ^ t >>> 7, 61 | t) ^ t; return ((t ^ t >>> 14) >>> 0) / 4294967296; };
const int = n => Math.floor(rand() * n);
const cases = [];
for (let c = 0; c < 400; c++) {
  const n = 1 + int(c < 200 ? 8 : 16);
  const density = rand() * 0.4;
  const deps = [];
  for (let i = 0; i < n; i++) {
    const d = [];
    for (let j = 0; j < n; j++) if (rand() < density) d.push(j);
    if (rand() < 0.1 && d.length) d.push(d[int(d.length)]); // duplicate edge
    // shuffle
    for (let k = d.length - 1; k > 0; k--) { const r = int(k + 1); [d[k], d[r]] = [d[r], d[k]]; }
    deps.push(d);
  }
  let included = [...Array(n).keys()];
  for (let k = n - 1; k > 0; k--) { const r = int(k + 1); [included[k], included[r]] = [included[r], included[k]]; }
  if (rand() < 0.3) included = included.slice(0, 1 + int(n));
  const graph = new Map(deps.map((d, i) => [i, d]));
  const { order, cycles } = graphSequencer(graph, included);
  cases.push({ deps, included, order, cycles });
}
// Larger graphs whose components need several rounds of cycle breaking, with
// some nodes listed twice in `included`.
for (let c = 0; c < 20; c++) {
  const n = 40 + int(31);
  const density = 0.02 + rand() * 0.1;
  const deps = [];
  for (let i = 0; i < n; i++) {
    const d = [];
    for (let j = 0; j < n; j++) if (rand() < density) d.push(j);
    deps.push(d);
  }
  const included = [...Array(n).keys()];
  for (let k = n - 1; k > 0; k--) { const r = int(k + 1); [included[k], included[r]] = [included[r], included[k]]; }
  for (let k = 0; k < 3; k++) included.push(included[int(n)]);
  const graph = new Map(deps.map((d, i) => [i, d]));
  const { order, cycles } = graphSequencer(graph, included);
  cases.push({ deps, included, order, cycles });
}
process.stdout.write(JSON.stringify(cases));
