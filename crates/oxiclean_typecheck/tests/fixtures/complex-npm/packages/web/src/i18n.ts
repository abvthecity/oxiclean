import en from "./locales/en.json";
import type { Paths } from "./paths";

export declare function t(key: Paths<typeof en>): string;

type Small = { a: { b: string; c: string }; d: string };
export declare function small(key: Paths<Small>): string;
