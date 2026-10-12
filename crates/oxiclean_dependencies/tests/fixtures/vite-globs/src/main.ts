export const pages = import.meta.glob('./pages/*.ts');

export const worker = new Worker(new URL('./worker.ts', import.meta.url), { type: 'module' });
