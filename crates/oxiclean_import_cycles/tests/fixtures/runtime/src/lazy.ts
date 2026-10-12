// lazy-target loads lazy only on demand: not a cycle unless includeDynamic is set.
import { target } from './lazy-target';

export const lazy = target;
