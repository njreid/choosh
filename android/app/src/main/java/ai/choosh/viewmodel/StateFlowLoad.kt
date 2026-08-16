package ai.choosh.viewmodel

import kotlinx.coroutines.flow.MutableStateFlow

/**
 * Runs [call] behind the "set loading, run a suspending call, land in success
 * or failure" shape that several ViewModels' `refresh()`/`load()` methods
 * otherwise repeat by hand — [ai.choosh.fleet.DevHostWorkspacesViewModel.refresh],
 * [ai.choosh.jj.JjDiffViewModel.load], and [ai.choosh.jj.JjChangeGraphViewModel.refresh]:
 * `copy(isLoading = true, error = null)`, then a
 * `runCatching { call() }.onSuccess { ... }.onFailure { ... }` pair that both
 * land back on `isLoading = false`. [setLoading]/[onSuccess]/[onFailure] each
 * take and return the caller's own `UiState` type, so every call site keeps
 * full control over which fields besides `isLoading`/`error` get touched.
 *
 * Deliberately not used everywhere a ViewModel loads something:
 * [ai.choosh.fleet.FleetViewModel.refresh]'s success path folds in
 * `sortMode`/a live `attentionTracker` signal via its own `recompute()`
 * rather than just writing the call's result into state, and
 * `ai.choosh.webservice.WebServiceViewModel.poll`/
 * `ai.choosh.sourceeditor.SourceEditorViewModel.load`/`saveNow` branch over
 * 4-5 richer sealed states rather than a plain success/failure pair — forcing
 * any of those through this shape would move complexity around rather than
 * reduce it.
 */
suspend fun <S, T> MutableStateFlow<S>.loadInto(
    setLoading: (S) -> S,
    call: suspend () -> T,
    onSuccess: (S, T) -> S,
    onFailure: (S, Throwable) -> S,
) {
    value = setLoading(value)
    runCatching { call() }
        .onSuccess { result -> value = onSuccess(value, result) }
        .onFailure { failure -> value = onFailure(value, failure) }
}
