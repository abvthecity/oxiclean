export enum Level {
  Low,
  High,
  /** @public */
  Kept,
  Unused,
}

// Re-exported by the entry, so every member is used.
export enum Mode {
  A,
  B,
}

export namespace Api {
  export const get = 1;
  export const put = 2;
}

// Never imported: an unused type, whose members aren't checked.
export enum Untouched {
  X,
}
