export * from "./classes";
export * from "./use";
export * from "./variants";
export interface Button {
    label: string;
    size: "sm" | "md" | "lg";
}
export declare function makeButton(label: string): Button;
