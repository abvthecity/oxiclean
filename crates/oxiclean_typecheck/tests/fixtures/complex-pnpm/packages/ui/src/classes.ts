import type { Color, Prefix, Shade, Size, State } from "./tokens";

export type Cls = `${State}${Prefix}-${Color}-${Shade}`;
type StripState<T> = T extends `${State}${infer R}` ? R : never;
export declare function cn<T extends Cls>(...c: T[]): StripState<T>;

// Small: 2 × 12 = 24 strings.
export type Badge = `${Size}-${Color}`;
