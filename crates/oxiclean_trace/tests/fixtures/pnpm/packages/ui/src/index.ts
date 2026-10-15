export * from "./classes";
export * from "./use";
export * from "./variants";

export interface Button {
  label: string;
  size: "sm" | "md" | "lg";
}

export function makeButton(label: string): Button {
  return { label, size: "md" };
}
