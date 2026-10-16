import en from "./locales/en.json";
import type { Paths } from "./paths";

export const t = (key: Paths<typeof en>): string => key;
