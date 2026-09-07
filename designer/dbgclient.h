// A Debug Adapter Protocol client for Studio's debugger.
//
// Studio speaks to the same adapter that VS Code's generic DAP client speaks
// to, rather than growing a private debugging path. Whatever the engine learns
// to do, every editor gets — including this one.
//
// The transport is `lspclient.h` again: the same spawn, the same non-blocking
// pipe drained once per frame, the same `Content-Length` framing, and the same
// `updated_`/`has_update()` pattern for answers that land between frames. That
// repetition is the point. Studio never traces the program itself and never
// waits on it: the adapter owns the debuggee, so a breakpoint hit costs this
// process nothing but a JSON message, where a `waitpid` on the UI thread would
// stall the frame loop at every stop and every single step.
//
// Two things differ from LSP and both are silent when got wrong:
//
//   - DAP is not JSON-RPC. A response carries `request_seq`, not `id`, and
//     events carry no sequence number worth matching on at all.
//   - The handshake is ordered. `launch` may not be sent before the answer to
//     `initialize`, and breakpoints may not be sent before the `initialized`
//     event — the adapter answers earlier ones unverified, which shows the
//     user a gutter full of hollow dots for breakpoints that would have bound
//     perfectly well. So the handshake is a state machine advanced from
//     `handle`, not a burst of messages in `start`.
#ifndef KILN_DESIGNER_DBGCLIENT_H
#define KILN_DESIGNER_DBGCLIENT_H

#include <fcntl.h>
#include <map>
#include <set>
#include <signal.h>
#include <string>
#ifndef _WIN32
#include <sys/wait.h>
#endif
#include <unistd.h>
#include <vector>

#include "json.h"
#include "portable.h"

namespace kiln::dbg {

/// The subcommand the adapter is started with.
///
/// It is spelled once, here, because the toolchain has not settled whether the
/// Debug Adapter Protocol is served by `kiln debug` or by an `kiln dap`
/// of its own. Changing this string is the whole of the change.
inline constexpr const char* ADAPTER_SUBCOMMAND = "dap";

/// The thread the adapter reports. It debugs one program with one thread of
/// user code, and DAP still requires a thread id on every stop.
inline constexpr int THREAD_ID = 1;

/// One frame of the call stack, as the Call Stack pane draws it.
struct Frame {
    /// The adapter's own id for the frame, which `scopes` and `evaluate` are
    /// asked in terms of. Never zero; some clients use zero for "no frame".
    int id = 0;
    std::string name;     ///< the subroutine as the user wrote it
    std::string source;   ///< absolute path, or "" when the adapter did not say
    int line = 0;         ///< 1-based, which is also the Code view's row number
    int column = 0;
};

/// One row of the Variables pane.
struct Variable {
    std::string name;
    std::string value;
    /// Non-zero when the value has parts the client may ask for with a
    /// `variables` request. Zero means a leaf, and a leaf drawn as expandable
    /// is a debugger that looks broken.
    int reference = 0;
};

/// What became of one breakpoint the adapter was asked for.
///
/// `verified` is why this is kept rather than thrown away: a solid dot means
/// the engine bound the line to an address and a hollow one means it did not,
/// and a breakpoint that silently never fires is the worst thing a first
/// version can do.
struct BreakpointStatus {
    int line = 0;          ///< where it ended up, which may not be where it was asked for
    bool verified = false;
    std::string message;   ///< why it did not bind, or where it moved to
};

/// A line the program or the build printed, and which stream it came from.
///
/// The category is the adapter's: `console` for the build's own words,
/// `stdout` and `stderr` for the program's, `important` for a failure the user
/// must not miss. Studio colours them differently, and conflating them makes a
/// compiler error look like something the program printed.
struct OutputLine {
    std::string category;
    std::string text;
};

/// Where the session is, which is what decides whether a toolbar button is
/// live or greyed.
enum class State {
    /// No adapter, or one that has not been started.
    Idle,
    /// Started: building, or launching, or waiting for the handshake to finish.
    Launching,
    /// The program is running and the adapter is not listening. Requests sent
    /// now are answered when it next stops, not before.
    Running,
    /// Stopped at a breakpoint, a step, a pause or a runtime error. This is the
    /// only state in which the stack and the locals mean anything.
    Stopped,
    /// The program has exited or the session was terminated. The adapter is
    /// still alive until `stop` disconnects it.
    Ended,
};

/// The text an `evaluate` answer carries, or "" when the adapter would not
/// evaluate it — an unknown name, or a stop that has since moved on. An empty
/// answer is what suppresses the hover tip, which is right: a wrong value is
/// worse than none.
inline std::string evaluate_text(const json::Value& body) { return body["result"].str(); }

class Client {
public:
    ~Client() { stop(); }

#ifdef _WIN32
    bool running() const { return child_.running(); }
#else
    bool running() const { return pid_ > 0; }
#endif

    /// Whether anything the panes draw has changed since `clear_update` — a
    /// stop, a resume, an end, a breakpoint answer or a line of output.
    ///
    /// A stop is never visible half-read, whatever else sets this: the stack
    /// and the locals are published in one step once both have landed.
    bool has_update() const { return updated_; }

    void clear_update() { updated_ = false; }

    State state() const { return state_; }

    /// The line the program is stopped on, or 0 when it is not stopped. 1-based,
    /// so it is the Code view's row number with no mapping in between.
    int stopped_line() const { return stopped_line_; }

    /// Why it stopped: `breakpoint`, `step`, `pause`, `entry` or `exception`.
    const std::string& stopped_reason() const { return stopped_reason_; }

    /// The runtime error behind an `exception` stop, or "" for every other
    /// reason. The user has to be shown this; it is the whole content of the
    /// stop.
    const std::string& stopped_detail() const { return stopped_detail_; }

    /// The user's frames, innermost first. Empty unless stopped.
    const std::vector<Frame>& frames() const { return frames_; }

    /// The innermost frame's locals. Empty unless stopped.
    const std::vector<Variable>& locals() const { return locals_; }

    /// What the last `set_breakpoints` bound, in the order it was asked for.
    const std::vector<BreakpointStatus>& breakpoints() const { return breakpoints_; }

    /// Completed output lines not yet taken. A line still being written stays
    /// in an internal buffer until its newline arrives, so a prompt printed
    /// without one is not shown twice.
    const std::vector<OutputLine>& output() const { return output_; }

    void clear_output() { output_.clear(); }

    /// The program's exit status, once `state` is `Ended` and it exited of its
    /// own accord. -1 when it was terminated or never ran.
    int exit_code() const { return exit_code_; }

    /// The reason the session could not start, or ended badly — a build
    /// diagnostic, or a program that could not be launched. "" when nothing
    /// has gone wrong.
    const std::string& error() const { return error_; }

    /// The `.kiln` the session is debugging, absolute.
    const std::string& program() const { return oir_; }

    /// Start the adapter on `oir_path` and begin the handshake.
    ///
    /// The adapter builds the program itself and reports the compiler's output
    /// as it arrives, so this must not be preceded by a build — but it must be
    /// preceded by a save, or the breakpoints are on lines the binary does not
    /// have.
    ///
    /// Returns as soon as the process is up. Nothing about the session is
    /// known yet; `poll` carries it the rest of the way.
    bool start(const std::string& kiln_bin, const std::string& oir_path) {
        stop();
        reset();
        const std::string real = kiln::sys::real_path(oir_path);
        oir_ = real.empty() ? oir_path : real;

#ifdef _WIN32
        // Both pipes, and the adapter's stderr to the null device: it logs
        // there, and a GUI program has no terminal for it to reach.
        if (!kiln::sys::spawn(child_,
                                 kiln::sys::quote_arg(kiln_bin) + " " + ADAPTER_SUBCOMMAND,
                                 false, true))
            return false;
#else
        int to_child[2], from_child[2];
        if (::pipe(to_child) != 0) return false;
        if (::pipe(from_child) != 0) {
            ::close(to_child[0]);
            ::close(to_child[1]);
            return false;
        }
        const pid_t pid = ::fork();
        if (pid == 0) {
            ::dup2(to_child[0], STDIN_FILENO);
            ::dup2(from_child[1], STDOUT_FILENO);
            ::close(to_child[0]);
            ::close(to_child[1]);
            ::close(from_child[0]);
            ::close(from_child[1]);
            // The adapter's own logging goes to stderr; let it reach the
            // terminal rather than mixing into the protocol stream, which one
            // stray byte desynchronises for good.
            ::execlp(kiln_bin.c_str(), kiln_bin.c_str(), ADAPTER_SUBCOMMAND, (char*)nullptr);
            _exit(127);
        }
        ::close(to_child[0]);
        ::close(from_child[1]);
        if (pid < 0) {
            ::close(to_child[1]);
            ::close(from_child[0]);
            return false;
        }
        in_ = to_child[1];
        out_ = from_child[0];
        ::fcntl(out_, F_SETFL, O_NONBLOCK);
        pid_ = pid;
#endif

        state_ = State::Launching;
        // Lines and columns pass through unconverted: gutter row N is `.kiln`
        // line N is the line the debug information names, and a mapping layer
        // between them would be one more thing to drift.
        initialize_seq_ = request("initialize",
                                  "{\"clientID\":\"kiln-studio\",\"adapterID\":\"kiln\","
                                  "\"linesStartAt1\":true,\"columnsStartAt1\":true,"
                                  "\"pathFormat\":\"path\",\"supportsVariableType\":false,"
                                  "\"supportsRunInTerminalRequest\":false}");
        return true;
    }

    /// Replace the breakpoint set with `lines`, which are 1-based `.kiln` lines.
    ///
    /// DAP replaces a file's whole set on every request, so this must always
    /// be called with every breakpoint Studio holds, never with the one that
    /// just changed. Calls made before the adapter is ready to be configured
    /// are kept and sent at the `initialized` event, which is the normal case:
    /// a user sets breakpoints and then presses Debug.
    void set_breakpoints(const std::vector<int>& lines) {
        lines_ = lines;
        if (running() && configured_) send_breakpoints();
    }

    /// Let the program go. Named with a trailing underscore because `continue`
    /// is a keyword.
    void continue_() {
        if (state_ != State::Stopped) return;
        forget_stop();
        fire("continue", "{\"threadId\":" + std::to_string(THREAD_ID) + "}");
    }

    void step_over() { step("next"); }
    void step_in() { step("stepIn"); }
    void step_out() { step("stepOut"); }

    /// Ask the program to stop.
    ///
    /// The adapter reads requests only between operations, so a pause sent
    /// while the program is running takes effect when it next stops rather
    /// than interrupting it. Studio must not wait for a `stopped` that may not
    /// come until the program reaches a breakpoint of its own.
    void pause() {
        if (state_ != State::Running && state_ != State::Launching) return;
        fire("pause", "{\"threadId\":" + std::to_string(THREAD_ID) + "}");
    }

    /// End the session and reap the adapter.
    ///
    /// `disconnect` is what tells the adapter to kill the debuggee it owns; a
    /// signal to the adapter alone would leave a traced process behind. The
    /// wait is on the *adapter*, never on the program — that separation is the
    /// reason the adapter is its own process.
    void stop() {
        if (!running()) {
            state_ = State::Idle;
            return;
        }
        fire("disconnect", "{\"restart\":false,\"terminateDebuggee\":true}");
#ifdef _WIN32
        kiln::sys::close_stdin(child_);
        int code = 0;
        for (int i = 0; i < 100 && !kiln::sys::try_wait(child_, code); i++) Sleep(10);
        if (!kiln::sys::try_wait(child_, code)) {
            kiln::sys::terminate(child_);
            WaitForSingleObject(child_.process, INFINITE);
        }
        kiln::sys::release(child_);
#else
        if (in_ >= 0) { ::close(in_); in_ = -1; }
        // Closing stdin is what lets the adapter's read loop finish; without
        // that it would sit waiting and we would wait for it.
        int status = 0;
        for (int i = 0; i < 100 && ::waitpid(pid_, &status, WNOHANG) == 0; i++) usleep(10000);
        if (::waitpid(pid_, &status, WNOHANG) == 0) {
            ::kill(pid_, SIGTERM);
            ::waitpid(pid_, &status, 0);
        }
        if (out_ >= 0) { ::close(out_); out_ = -1; }
        pid_ = 0;
#endif
        state_ = State::Idle;
        stopped_line_ = 0;
        frames_.clear();
        locals_.clear();
    }

    /// Ask what `expression` is worth in the innermost frame, and return the
    /// sequence number its answer will arrive under.
    ///
    /// Zero when there is nothing to ask. The answer is taken with
    /// `take_response` when it lands, rather than waited for: pumping the pipe
    /// on every mouse move is exactly the stutter the non-blocking drain
    /// exists to avoid.
    int evaluate(const std::string& expression) {
        if (state_ != State::Stopped || expression.empty()) return 0;
        return request("evaluate", "{\"expression\":\"" + json::escape(expression) +
                                       "\",\"frameId\":" + std::to_string(frame_id_) +
                                       ",\"context\":\"hover\"}");
    }

    /// Send a request whose answer the caller will take with `take_response`,
    /// and return the sequence number it will come back under in
    /// `request_seq`.
    int request(const std::string& command, const std::string& arguments_json) {
        if (!running()) return 0;
        const int seq = next_seq_++;
        send("{\"seq\":" + std::to_string(seq) + ",\"type\":\"request\",\"command\":\"" + command +
             "\",\"arguments\":" + arguments_json + "}");
        return seq;
    }

    /// The body of the answer to `seq`, once. False until it has arrived. A
    /// failed request counts as arrived, with the adapter's error in the body:
    /// a request that failed is not a request still in flight.
    bool take_response(int seq, json::Value& out) {
        auto it = responses_.find(seq);
        if (it == responses_.end()) return false;
        out = std::move(it->second);
        responses_.erase(it);
        return true;
    }

    /// Read whatever the adapter has sent. Call once per frame; never blocks.
    void poll() {
        if (!output_open()) return;
        char buf[4096];
#ifdef _WIN32
        int n;
        while ((n = kiln::sys::read_nonblocking(child_.out, buf, sizeof buf)) > 0)
            inbuf_.append(buf, (size_t)n);
        if (n == 0) kiln::sys::close_output(child_);   // the adapter closed its end
#else
        ssize_t n;
        while ((n = ::read(out_, buf, sizeof buf)) > 0) inbuf_.append(buf, (size_t)n);
        if (n == 0) {                    // the adapter closed its end
            ::close(out_);
            out_ = -1;
        }
#endif

        // Frames are `Content-Length: N\r\n\r\n<N bytes>`; anything short is
        // left in the buffer for the next poll rather than mis-parsed.
        for (;;) {
            const size_t head = inbuf_.find("\r\n\r\n");
            if (head == std::string::npos) return;
            const size_t cl = inbuf_.find("Content-Length:");
            if (cl == std::string::npos || cl > head) {
                inbuf_.erase(0, head + 4);
                continue;
            }
            const size_t len = (size_t)std::atoi(inbuf_.c_str() + cl + 15);
            const size_t body = head + 4;
            if (inbuf_.size() < body + len) return;   // wait for the rest
            handle(inbuf_.substr(body, len));
            inbuf_.erase(0, body + len);
        }
    }

    /// Wait up to `ms` for the answer to `seq`, pumping the pipe meanwhile.
    /// For scripted sessions, where there is no frame loop to come back to.
    /// Events arriving on the way are handled, not dropped.
    bool wait(int seq, json::Value& out, int ms) {
        return pump_until(seq, ms) && take_response(seq, out);
    }

    /// As `wait`, but the answer stays queued for whoever normally takes it.
    bool pump_until(int seq, int ms) {
        for (int i = 0; i < ms / 10; i++) {
            poll();
            if (responses_.count(seq)) return true;
            if (!output_open()) return false;
            usleep(10000);
        }
        return responses_.count(seq) > 0;
    }

    /// Wait up to `ms` for the program to stop, or to end. False if neither
    /// happened in time. This is what the headless harness's `waitstop` is,
    /// and it is the one place blocking is right: there is no frame to return
    /// to in a scripted run.
    bool wait_for_stop(int ms) {
        for (int i = 0; i < ms / 10; i++) {
            poll();
            if (state_ == State::Stopped || state_ == State::Ended) return true;
            if (!output_open()) return false;
            usleep(10000);
        }
        poll();
        return state_ == State::Stopped || state_ == State::Ended;
    }

private:
#ifdef _WIN32
    bool output_open() const { return child_.out != nullptr; }
#else
    bool output_open() const { return out_ >= 0; }
#endif

    /// Everything that is about one session rather than about the client.
    void reset() {
        state_ = State::Idle;
        configured_ = false;
        updated_ = false;
        stopped_line_ = 0;
        frame_id_ = 1;
        pending_line_ = 0;
        pending_frame_id_ = 1;
        exit_code_ = -1;
        next_seq_ = 1;
        initialize_seq_ = launch_seq_ = breakpoints_seq_ = 0;
        stack_seq_ = scopes_seq_ = variables_seq_ = 0;
        stopped_reason_.clear();
        stopped_detail_.clear();
        error_.clear();
        inbuf_.clear();
        frames_.clear();
        locals_.clear();
        pending_frames_.clear();
        pending_locals_.clear();
        breakpoints_.clear();
        output_.clear();
        partial_.clear();
        responses_.clear();
        ignored_.clear();
    }

    void send(const std::string& body) {
        const std::string frame =
            "Content-Length: " + std::to_string(body.size()) + "\r\n\r\n" + body;
#ifdef _WIN32
        if (!child_.in) return;
        if (!kiln::sys::write_all(child_.in, frame.data(), frame.size()))
            kiln::sys::close_stdin(child_);   // the adapter died; drop the pipe
        return;
#else
        if (in_ < 0) return;
        // A short write would corrupt the frame, so keep going until it is all
        // out. EPIPE means the adapter died; drop the pipe rather than loop.
        size_t sent = 0;
        while (sent < frame.size()) {
            const ssize_t w = ::write(in_, frame.data() + sent, frame.size() - sent);
            if (w <= 0) {
                ::close(in_);
                in_ = -1;
                return;
            }
            sent += (size_t)w;
        }
#endif
    }

    void step(const char* command) {
        if (state_ != State::Stopped) return;
        forget_stop();
        fire(command, "{\"threadId\":" + std::to_string(THREAD_ID) + "}");
    }

    /// Send a request whose answer is of no interest, and remember that.
    ///
    /// A stepping session sends thousands of these, and keeping every reply
    /// against a sequence number nobody will ever ask for is a map that grows
    /// for as long as the session lasts.
    void fire(const std::string& command, const std::string& arguments_json) {
        const int seq = request(command, arguments_json);
        if (seq != 0) ignored_.insert(seq);
    }

    /// Let go of everything that described where the program was.
    ///
    /// The adapter invalidates every `variablesReference` the moment the
    /// program moves, so holding the previous stop's stack would show the user
    /// values that are no longer true — and the stopped-line tint would sit on
    /// a line the program has left.
    void forget_stop() {
        state_ = State::Running;
        stopped_line_ = 0;
        stopped_reason_.clear();
        stopped_detail_.clear();
        frames_.clear();
        locals_.clear();
        pending_frames_.clear();
        pending_locals_.clear();
        pending_line_ = 0;
        stack_seq_ = scopes_seq_ = variables_seq_ = 0;
        updated_ = true;
    }

    /// The whole breakpoint set, which is the only form DAP accepts.
    void send_breakpoints() {
        std::string list;
        for (size_t i = 0; i < lines_.size(); i++) {
            if (i) list += ",";
            list += "{\"line\":" + std::to_string(lines_[i]) + "}";
        }
        breakpoints_seq_ =
            request("setBreakpoints", "{\"source\":{\"path\":\"" + json::escape(oir_) +
                                          "\"},\"breakpoints\":[" + list + "]}");
    }

    void handle(const std::string& body) {
        const json::Value msg = json::parse(body);
        const std::string type = msg["type"].str();
        if (type == "response") {
            handle_response(msg);
        } else if (type == "event") {
            handle_event(msg["event"].str(), msg["body"]);
        }
    }

    void handle_response(const json::Value& msg) {
        // The request's number comes back in `request_seq` and nowhere else.
        // Reading `seq` here is the mistake anyone arriving from an LSP server
        // makes, and it matches nothing: `seq` is the adapter's own counter.
        const int seq = msg["request_seq"].num(0);
        // Read the flag the protocol defines rather than inferring one from
        // the absence of an error, which would call a failure with nothing to
        // say about itself a success.
        const json::Value& flag = msg["success"];
        const bool ok = flag.kind == json::Value::Kind::Bool && flag.boolean;
        const json::Value& body = msg["body"];
        if (seq != 0 && ignored_.erase(seq)) return;

        if (seq != 0 && seq == initialize_seq_) {
            initialize_seq_ = 0;
            // Only now may `launch` be sent. The adapter builds the program
            // and answers this request last of all, after configuration is
            // done, so nothing here waits for it.
            const size_t slash = oir_.find_last_of('/');
            const std::string dir = slash == std::string::npos ? "." : oir_.substr(0, slash);
            launch_seq_ = request("launch", "{\"program\":\"" + json::escape(oir_) +
                                                "\",\"cwd\":\"" + json::escape(dir) +
                                                "\",\"stopOnEntry\":false}");
            return;
        }
        if (seq != 0 && seq == launch_seq_) {
            launch_seq_ = 0;
            if (!ok) {
                // A build diagnostic arrives here as well as in the `output`
                // events that scrolled past, because this is the one copy the
                // user is guaranteed to still be looking at.
                error_ = body["error"]["format"].str(msg["message"].str("the launch failed"));
                state_ = State::Ended;
                updated_ = true;
            }
            return;
        }
        if (seq != 0 && seq == breakpoints_seq_) {
            breakpoints_seq_ = 0;
            breakpoints_.clear();
            const json::Value& list = body["breakpoints"];
            for (size_t i = 0; i < list.size(); i++) {
                const json::Value& b = list.at(i);
                BreakpointStatus status;
                status.line = b["line"].num(i < lines_.size() ? lines_[i] : 0);
                status.verified = b["verified"].kind == json::Value::Kind::Bool &&
                                  b["verified"].boolean;
                status.message = b["message"].str();
                breakpoints_.push_back(std::move(status));
            }
            updated_ = true;
            return;
        }
        if (seq != 0 && seq == stack_seq_) {
            stack_seq_ = 0;
            read_stack(body);
            return;
        }
        if (seq != 0 && seq == scopes_seq_) {
            scopes_seq_ = 0;
            // The first scope is Locals; the adapter offers no other, and
            // asking for a reference it did not hand out is an error.
            const int reference = body["scopes"].at(0)["variablesReference"].num(0);
            if (reference == 0) {
                finish_stop();
                return;
            }
            variables_seq_ =
                request("variables", "{\"variablesReference\":" + std::to_string(reference) + "}");
            return;
        }
        if (seq != 0 && seq == variables_seq_) {
            variables_seq_ = 0;
            pending_locals_.clear();
            const json::Value& list = body["variables"];
            for (size_t i = 0; i < list.size(); i++) {
                const json::Value& v = list.at(i);
                Variable var;
                var.name = v["name"].str();
                var.value = v["value"].str();
                var.reference = v["variablesReference"].num(0);
                pending_locals_.push_back(std::move(var));
            }
            finish_stop();
            return;
        }
        // Anything else was asked for by Studio itself — a hover, or a value
        // the user expanded — and is handed over by `take_response`.
        if (seq != 0) responses_[seq] = body;
    }

    void handle_event(const std::string& name, const json::Value& body) {
        if (name == "initialized") {
            // The adapter is ready to be configured, which is the first moment
            // a breakpoint can bind to anything. The set is sent even when it
            // is empty: `configurationDone` must follow it, and the adapter
            // holds the launch answer until it arrives.
            configured_ = true;
            send_breakpoints();
            fire("configurationDone", "{}");
            return;
        }
        if (name == "stopped") {
            state_ = State::Stopped;
            stopped_reason_ = body["reason"].str("stopped");
            // Clients differ about which field they read, so the adapter fills
            // both and so does this.
            stopped_detail_ = body["description"].str(body["text"].str());
            frames_.clear();
            locals_.clear();
            pending_frames_.clear();
            pending_locals_.clear();
            stopped_line_ = 0;
            pending_line_ = 0;
            pending_frame_id_ = 1;
            // A stop can arrive while the previous one is still being read —
            // a pause landing mid-chain does exactly that — and the answers
            // still in flight describe where the program was. Forgetting their
            // sequence numbers is what stops them completing a stop that has
            // already been superseded.
            stack_seq_ = scopes_seq_ = variables_seq_ = 0;
            // The stop says nothing about *where*; that is the innermost
            // frame's line, so the stack is asked for straight away and the
            // scopes and locals follow from its answer. `updated_` waits until
            // the whole chain has landed, so the panes are never drawn half
            // filled.
            stack_seq_ = request("stackTrace", "{\"threadId\":" + std::to_string(THREAD_ID) + "}");
            return;
        }
        if (name == "continued") {
            forget_stop();
            return;
        }
        if (name == "exited") {
            exit_code_ = body["exitCode"].num(0);
            return;
        }
        if (name == "terminated") {
            // This arrives in every state, including from a build that never
            // produced a program, so it must not assume there was a session.
            state_ = State::Ended;
            stopped_line_ = 0;
            frames_.clear();
            locals_.clear();
            updated_ = true;
            return;
        }
        if (name == "output") {
            take_output(body["category"].str("console"), body["output"].str());
            return;
        }
    }

    void read_stack(const json::Value& body) {
        pending_frames_.clear();
        const json::Value& list = body["stackFrames"];
        for (size_t i = 0; i < list.size(); i++) {
            const json::Value& f = list.at(i);
            Frame frame;
            frame.id = f["id"].num((int)i + 1);
            frame.name = f["name"].str();
            frame.source = f["source"]["path"].str();
            frame.line = f["line"].num(0);
            frame.column = f["column"].num(0);
            pending_frames_.push_back(std::move(frame));
        }
        if (pending_frames_.empty()) {
            // A stop with no user frames is a stop somewhere the debugger
            // cannot describe. There is nothing to read locals from, so the
            // chain ends here rather than asking about a frame that is not
            // there.
            finish_stop();
            return;
        }
        pending_line_ = pending_frames_.front().line;
        pending_frame_id_ = pending_frames_.front().id;
        scopes_seq_ =
            request("scopes", "{\"frameId\":" + std::to_string(pending_frame_id_) + "}");
    }

    /// Publish a stop that has been read all the way through.
    ///
    /// The stack and the locals become visible together, in one step, because
    /// anything else lets a frame land between the two answers and paint a
    /// stopped line over an empty Variables pane. `has_update` is not enough
    /// on its own to prevent that: an `output` event arriving mid-chain would
    /// set it while the stop was still half read.
    void finish_stop() {
        frames_ = std::move(pending_frames_);
        locals_ = std::move(pending_locals_);
        pending_frames_.clear();
        pending_locals_.clear();
        stopped_line_ = pending_line_;
        frame_id_ = pending_frame_id_;
        updated_ = true;
    }

    /// Buffer output until a line is whole.
    ///
    /// The build's output arrives a line at a time, but the program's arrives
    /// as whatever the pipe happened to hold, so a line can be split across
    /// two events. Surfacing the halves separately would show the user two
    /// broken lines and no way to tell that is what happened.
    void take_output(const std::string& category, const std::string& text) {
        if (text.empty()) return;
        std::string& tail = partial_[category];
        tail += text;
        size_t start = 0;
        for (;;) {
            const size_t nl = tail.find('\n', start);
            if (nl == std::string::npos) break;
            size_t end = nl;
            if (end > start && tail[end - 1] == '\r') end--;
            output_.push_back({category, tail.substr(start, end - start)});
            start = nl + 1;
        }
        tail.erase(0, start);
        updated_ = true;
    }

#ifdef _WIN32
    kiln::sys::Child child_;
#else
    pid_t pid_ = 0;
    int in_ = -1;
    int out_ = -1;
#endif
    State state_ = State::Idle;
    /// Whether the `initialized` event has arrived, which is what makes a
    /// breakpoint request worth sending.
    bool configured_ = false;
    bool updated_ = false;
    int next_seq_ = 1;
    int initialize_seq_ = 0;
    int launch_seq_ = 0;
    int breakpoints_seq_ = 0;
    int stack_seq_ = 0;
    int scopes_seq_ = 0;
    int variables_seq_ = 0;
    int frame_id_ = 1;
    int stopped_line_ = 0;
    /// Where the stop being read says it is. Kept apart from what the panes
    /// draw until every answer of the chain has landed.
    int pending_line_ = 0;
    int pending_frame_id_ = 1;
    int exit_code_ = -1;
    std::string oir_;
    std::string inbuf_;
    std::string stopped_reason_;
    std::string stopped_detail_;
    std::string error_;
    /// The breakpoint set, which deliberately outlives a session: a
    /// breakpoint is a fact about debugging this program, and it persists
    /// across a rebuild for as long as Studio is open.
    std::vector<int> lines_;
    std::vector<Frame> frames_;
    std::vector<Variable> locals_;
    std::vector<Frame> pending_frames_;
    std::vector<Variable> pending_locals_;
    std::vector<BreakpointStatus> breakpoints_;
    std::vector<OutputLine> output_;
    std::map<std::string, std::string> partial_;
    std::map<int, json::Value> responses_;
    std::set<int> ignored_;
};

} // namespace kiln::dbg

#endif
