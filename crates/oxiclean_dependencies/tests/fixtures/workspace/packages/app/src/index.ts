import { Buffer } from 'buffer';
import { createRoot } from 'react';
import { Button } from '@acme/ui';
// Listed only in the root package.json, which Node finds next.
import { z } from 'zod';

export const app = [Buffer, createRoot, Button, z];
