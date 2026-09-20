/** Command namespace shared by every native IPC call of the plugin. */
export const COMMAND = 'plugin:video|'

export interface WebView2TextureStreamApi {
  getTextureStream(streamId: string): Promise<MediaStream>
}

export function webView2TextureStream(): WebView2TextureStreamApi | undefined {
  const scope = globalThis as typeof globalThis & {
    chrome?: { webview?: Partial<WebView2TextureStreamApi> }
  }
  const getTextureStream = scope.chrome?.webview?.getTextureStream
  return typeof getTextureStream === 'function'
    ? { getTextureStream: getTextureStream.bind(scope.chrome?.webview) }
    : undefined
}
