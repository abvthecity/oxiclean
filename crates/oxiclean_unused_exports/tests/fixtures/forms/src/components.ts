export interface Props {
  label: string;
}
// Used through the signature of Button, which is used.
export interface ButtonStyle {
  color: string;
}
export function Button(props: Props, style?: ButtonStyle) {
  return [props.label, style];
}
export const used = () => 1;
export const unused = 2;
export type UnusedType = string;
/** @public */
export const publicApi = 3;
/** @beta */
export const betaApi = 4;
/** @internal */
export const internalApi = 5;
export enum Color {
  Red,
}
