// `b.ts` reads members through the barrel before `z.ts` uses the enum by
// name; the order must not matter.
import './b';
import './z';
import * as ns from './dotted';

console.log(ns['a.b']);
