// Intersections of unions are normalised to their product: 20^3 members.
type A =
  | { a0: true }
  | { a1: true }
  | { a2: true }
  | { a3: true }
  | { a4: true }
  | { a5: true }
  | { a6: true }
  | { a7: true }
  | { a8: true }
  | { a9: true }
  | { a10: true }
  | { a11: true }
  | { a12: true }
  | { a13: true }
  | { a14: true }
  | { a15: true }
  | { a16: true }
  | { a17: true }
  | { a18: true }
  | { a19: true };
type B =
  | { b0: true }
  | { b1: true }
  | { b2: true }
  | { b3: true }
  | { b4: true }
  | { b5: true }
  | { b6: true }
  | { b7: true }
  | { b8: true }
  | { b9: true }
  | { b10: true }
  | { b11: true }
  | { b12: true }
  | { b13: true }
  | { b14: true }
  | { b15: true }
  | { b16: true }
  | { b17: true }
  | { b18: true }
  | { b19: true };
type C =
  | { c0: true }
  | { c1: true }
  | { c2: true }
  | { c3: true }
  | { c4: true }
  | { c5: true }
  | { c6: true }
  | { c7: true }
  | { c8: true }
  | { c9: true }
  | { c10: true }
  | { c11: true }
  | { c12: true }
  | { c13: true }
  | { c14: true }
  | { c15: true }
  | { c16: true }
  | { c17: true }
  | { c18: true }
  | { c19: true };
export type Variant = A & B & C;
export function widen(v: Variant): A {
  const a: A = v;
  return a;
}
