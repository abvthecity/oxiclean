import { used, type Props, Button } from './components';
import * as utils from './utils';
import { icons } from './barrel';
import legacy from './legacy';
import './augment';
import { helped } from './helpers';
import { Level } from './members-barrel';
import { Api } from './members';

// An entry's re-exports are its interface, so they count as used.
export { fromEntry } from './re-exported';
export { Mode } from './members';

utils.format();
console.log(icons.Star);
used();
Button({} as Props);
legacy();
helped;
console.log(Level.Low, Api.get);

const { lazyUsed } = await import('./lazy');
lazyUsed();
