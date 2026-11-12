export const MIN_MAINTAINER_TOKEN_CHARS = 32;

export const MAINTAINER_TOKEN_UNAVAILABLE =
  `Maintainer sign-in is unavailable: MAINTAINER_TOKEN must be set to a secret of at least ${MIN_MAINTAINER_TOKEN_CHARS} characters.`;

export function maintainerTokenUsable(token: string | undefined): token is string {
  return typeof token === "string" && token.length >= MIN_MAINTAINER_TOKEN_CHARS;
}
