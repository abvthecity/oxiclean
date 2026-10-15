import strings from "./strings.json";
import typed from "./typed.json";

export const small = { a: 1, b: [1, 2] };

export const icons = {
  home: { path: "M0 0h24v24H0z", size: 24 },
  user: { path: "M12 12c2.2 0 4-1.8 4-4", size: 24 },
  gear: { path: "M19.1 12.9a7 7 0 0 0 0-1.8", size: 24 },
  bell: { path: "M12 22c1.1 0 2-.9 2-2h-4", size: 24 },
  star: { path: "M12 17.3l6.2 3.7-1.6-7", size: 24 },
  flag: { path: "M14.4 6L14 4H5v17h2v-7", size: 24 },
  lock: { path: "M18 8h-1V6c0-2.8-2.2-5-5", size: 24 },
} as const;

export function label(key: keyof typeof strings): string {
  return strings[key] ?? typed[key];
}
