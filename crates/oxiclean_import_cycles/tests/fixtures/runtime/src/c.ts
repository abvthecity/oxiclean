// d only imports a type from c, which is erased at runtime: not a cycle.
import { d } from './d';

export interface Shape {
  sides: number;
}
export const c = d;
