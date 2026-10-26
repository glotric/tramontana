/** The error notice every screen shows for a failed call; nothing when `text` is undefined. */
export function Failure({ text }: { readonly text: string | undefined }): React.JSX.Element | null {
  return text === undefined ? null : (
    <p className="notice notice--error" role="alert">
      {text}
    </p>
  );
}
