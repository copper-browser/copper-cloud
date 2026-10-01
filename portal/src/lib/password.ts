// No 0/O, 1/l/I: the password is often read off a screen and typed elsewhere.
const ALPHABET = "abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/**
 * A strong, readable password: four dash-separated groups of five characters
 * from a 56-symbol alphabet (~116 bits). Unbiased rejection sampling over
 * `crypto.getRandomValues`.
 */
export function generatePassword(groups = 4, groupSize = 5): string {
  const total = groups * groupSize;
  const out: string[] = [];
  const limit = 256 - (256 % ALPHABET.length);
  while (out.length < total) {
    const bytes = crypto.getRandomValues(new Uint8Array(total * 2));
    for (const b of bytes) {
      if (b < limit) out.push(ALPHABET[b % ALPHABET.length]!);
      if (out.length === total) break;
    }
  }
  const chunks: string[] = [];
  for (let i = 0; i < total; i += groupSize)
    chunks.push(out.slice(i, i + groupSize).join(""));
  return chunks.join("-");
}

export const MIN_PASSWORD_LENGTH = 10;
