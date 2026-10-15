import data from "./data.json" with { type: "json" };

Deno.serve(() => Response.json(data));
