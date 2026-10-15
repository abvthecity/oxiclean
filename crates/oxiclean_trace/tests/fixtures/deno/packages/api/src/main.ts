import { z } from "zod";
import { form } from "zod-form";
import type { Shape } from "./Schema";

export const user = z
  .object({ id: z.number() })
  .extend({ f1: z.string() })
  .extend({ f2: z.string() })
  .extend({ f3: z.string() })
  .extend({ f4: z.string() })
  .extend({ f5: z.string() })
  .extend({ f6: z.string() })
  .extend({ f7: z.string() })
  .extend({ f8: z.string() })
  .extend({ f9: z.string() })
  .extend({ f10: z.string() })
  .extend({ f11: z.string() })
  .extend({ f12: z.string() })
  .extend({ f13: z.string() })
  .extend({ f14: z.string() })
  .extend({ f15: z.string() })
  .extend({ f16: z.string() })
  .extend({ f17: z.string() })
  .extend({ f18: z.string() })
  .extend({ f19: z.string() })
  .extend({ f20: z.string() })
  .extend({ f21: z.string() })
  .extend({ f22: z.string() })
  .extend({ f23: z.string() })
  .extend({ f24: z.string() })
  .extend({ f25: z.string() })
  .extend({ f26: z.string() })
  .extend({ f27: z.string() })
  .extend({ f28: z.string() })
  .extend({ f29: z.string() })
  .extend({ f30: z.string() })
  .extend({ f31: z.string() })
  .extend({ f32: z.string() })
  .extend({ f33: z.string() })
  .extend({ f34: z.string() })
  .extend({ f35: z.string() })
  .extend({ f36: z.string() })
  .extend({ f37: z.string() })
  .extend({ f38: z.string() })
  .extend({ f39: z.string() })
  .extend({ f40: z.string() });

export const shape: Shape = { id: 1 };
export const login = form(z.object({ name: z.string() }));
