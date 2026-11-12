/** 0 to 4: length past the minimum and the mix of character kinds. A hint only; the server's rule is the length. */
export function passwordScore(value: string, min: number): number {
  if (value.length === 0) return 0;
  if (value.length < min) return 1;
  const kinds = [/[a-z]/, /[A-Z]/, /\d/, /[^A-Za-z0-9]/].filter((r) => r.test(value)).length;
  let score = 2;
  if (value.length >= min + 6 || kinds >= 3) score++;
  if (value.length >= min + 10 || (value.length >= min + 4 && kinds >= 3)) score++;
  return Math.min(score, 4);
}
