type Tone = { tone0: true } | { tone1: true } | { tone2: true } | { tone3: true } | { tone4: true } | { tone5: true } | { tone6: true } | { tone7: true } | { tone8: true } | { tone9: true } | { tone10: true } | { tone11: true };
type Density = { density0: true } | { density1: true } | { density2: true } | { density3: true } | { density4: true } | { density5: true } | { density6: true } | { density7: true } | { density8: true } | { density9: true };
type Shape = { shape0: true } | { shape1: true } | { shape2: true } | { shape3: true } | { shape4: true } | { shape5: true } | { shape6: true } | { shape7: true } | { shape8: true } | { shape9: true };

export const defaults: Tone & Density & Shape = { tone3: true, density4: true, shape5: true };
