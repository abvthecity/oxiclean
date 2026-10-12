// a and b import each other at runtime: a cycle.
import { b } from './b';

export const a = () => b();
