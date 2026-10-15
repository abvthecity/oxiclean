export type Range<N extends number, A extends number[] = []> = A["length"] extends N
  ? A[number]
  : Range<N, [...A, A["length"]]>;

export type Tup<N extends number, A extends unknown[] = []> = A["length"] extends N
  ? A
  : Tup<N, [...A, unknown]>;
