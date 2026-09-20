import type { MediaInfo, VideoPluginError } from '@get-air/video'

import type { NativePlaybackSnapshot } from './models'
import { errorMessage } from './protocol'

/**
 * Maps a native playback snapshot onto the shared `MediaInfo` cache. The
 * media object is mutated in place so long-lived readers keep observing
 * the same instance the controller exposes.
 */
export function updateNativeMedia(media: MediaInfo, snapshot: NativePlaybackSnapshot): void {
  const live = snapshot.live ?? false
  media.durationSeconds = live ? undefined : snapshot.durationSeconds
  media.seekable = snapshot.seekable ?? !live
  media.seekableStartSeconds = media.seekable
    ? snapshot.seekableStartSeconds ?? 0
    : undefined
  media.seekableEndSeconds = media.seekable
    ? snapshot.seekableEndSeconds ?? (live ? undefined : snapshot.durationSeconds)
    : undefined
  media.live = live
  media.container = snapshot.container ?? 'unknown'
  const tracksChanged = snapshot.tracks.length !== media.tracks.length
    || snapshot.tracks.some((track, index) => {
      const cached = media.tracks[index]
      return !cached
        || cached.id !== track.id
        || cached.kind !== track.kind
        || cached.streamIndex !== track.index
        || cached.codec !== track.codec
        || cached.label !== track.label
        || cached.language !== track.language
        || cached.selected !== track.selected
        || (track.kind === 'video' && (
          cached.width !== snapshot.videoWidth || cached.height !== snapshot.videoHeight
        ))
    })
  if (tracksChanged) {
    media.tracks = snapshot.tracks.map((track) => ({
      id: track.id,
      kind: track.kind,
      streamIndex: track.index,
      codec: track.codec,
      caps: track.codec,
      label: track.label,
      language: track.language,
      selected: track.selected,
      default: false,
      forced: false,
      width: track.kind === 'video' ? snapshot.videoWidth : undefined,
      height: track.kind === 'video' ? snapshot.videoHeight : undefined,
    }))
  }
}

/** A finite timeline is ended once playout reaches its reported duration. */
export function hasEnded(snapshot: NativePlaybackSnapshot | undefined): boolean {
  return Boolean(snapshot
    && !snapshot.live
    && snapshot.durationSeconds > 0
    && snapshot.currentTimeSeconds >= snapshot.durationSeconds)
}

export function normalizeError(error: unknown): VideoPluginError {
  if (typeof error === 'object' && error && 'code' in error && 'message' in error) {
    return error as VideoPluginError
  }
  return { code: 'transport', message: errorMessage(error) }
}
