package ai.choosh.fleet

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.rules.TestWatcher
import org.junit.runner.Description

/**
 * Swaps `Dispatchers.Main` for a [TestDispatcher] around each test — needed
 * because [FleetViewModel] uses `viewModelScope` (`Dispatchers.Main`
 * underneath), which has no real Android `Looper` to back it in a plain JVM
 * unit test. Standard `kotlinx-coroutines-test` idiom; not production code.
 *
 * [dispatcher] is exposed so a test can pass it into `runTest(dispatcher)`,
 * sharing the *same* [kotlinx.coroutines.test.TestCoroutineScheduler] between
 * the test body and anything `viewModelScope.launch` schedules on Main — two
 * independently-constructed `TestDispatcher`s have independent virtual
 * clocks, so without this sharing a coroutine suspended on `Main` (e.g. in
 * [ai.choosh.engine.FakeChooshEngine]'s `delay()` calls) never gets advanced
 * by `runTest`'s own idle-checking and simply never completes.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class MainDispatcherRule(val dispatcher: TestDispatcher = StandardTestDispatcher()) : TestWatcher() {
    override fun starting(description: Description) {
        Dispatchers.setMain(dispatcher)
    }

    override fun finished(description: Description) {
        Dispatchers.resetMain()
    }
}
