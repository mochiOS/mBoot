package HvConfig;

use strict;
use warnings;
use Exporter 'import';

our @EXPORT_OK = qw(read_hv_config role_id);

sub read_hv_config {
    my ($path) = @_;
    open my $fh, '<', $path or die "cannot read $path: $!\n";

    my %root;
    my @domains;
    my @channels;
    my $current = \%root;
    my $line_number = 0;
    while (my $line = <$fh>) {
        ++$line_number;
        chomp $line;
        $line =~ s/^\s+|\s+$//g;
        next if $line eq '' || $line =~ /^#/;
        if ($line eq '[[domains]]') {
            push @domains, {};
            $current = $domains[-1];
            next;
        }
        if ($line eq '[[channels]]') {
            push @channels, {};
            $current = $channels[-1];
            next;
        }
        $line =~ /^([a-z][a-z0-9_]*)\s*=\s*(.+)$/
            or die "$path:$line_number: invalid config line\n";
        my ($key, $raw) = ($1, $2);
        exists $current->{$key}
            and die "$path:$line_number: duplicate key $key\n";
        $current->{$key} = parse_value($raw, $path, $line_number);
    }
    close $fh or die "cannot close $path: $!\n";

    for my $key (qw(version toolchain disk_size_mib esp_size_mib disk_guid esp_guid)) {
        exists $root{$key} or die "$path: missing $key\n";
    }
    $root{version} == 3 or die "$path: unsupported version $root{version}\n";
    $root{disk_size_mib} > $root{esp_size_mib} + 2
        or die "$path: disk_size_mib must exceed esp_size_mib by at least 2 MiB\n";
    @domains && @domains <= 8 or die "$path: domains must contain 1 to 8 entries\n";

    my %ids;
    my $system_domains = 0;
    for my $domain (@domains) {
        for my $key (qw(id role memory_mib vcpus capabilities image path autostart required restart max_restarts)) {
            exists $domain->{$key} or die "$path: Domain is missing $key\n";
        }
        $domain->{id} > 0 && !$ids{$domain->{id}}++
            or die "$path: Domain IDs must be nonzero and unique\n";
        role_id($domain->{role});
        ++$system_domains if $domain->{role} eq 'system';
        $domain->{memory_mib} > 0 && $domain->{memory_mib} <= 2
            or die "$path: current Domain memory_mib range is 1 to 2\n";
        $domain->{vcpus} == 1
            or die "$path: current hypervisor supports one vCPU per Domain\n";
        $domain->{path} =~ m{^\\EFI\\MBOOT\\[A-Za-z0-9._-]+$}
            or die "$path: Domain path must stay below \\EFI\\MBOOT\n";
        $domain->{autostart}
            or die "$path: current bootstrap requires autostart Domains\n";
        $domain->{restart} =~ /^(?:never|on-failure|always)$/
            or die "$path: invalid Domain restart policy\n";
        if ($domain->{restart} eq 'never') {
            $domain->{max_restarts} == 0
                or die "$path: never-restarted Domain must use max_restarts = 0\n";
        }
        else {
            $domain->{max_restarts} > 0 && $domain->{max_restarts} <= 255
                or die "$path: restarted Domain max_restarts must be 1 to 255\n";
        }
    }
    $system_domains == 1 or die "$path: exactly one System Domain is required\n";

    @channels <= 64 or die "$path: channels may contain at most 64 entries\n";
    my %endpoints;
    for my $channel (@channels) {
        for my $key (qw(domain_a port_a domain_b port_b)) {
            exists $channel->{$key} or die "$path: Event Channel is missing $key\n";
        }
        $channel->{domain_a} != $channel->{domain_b}
            or die "$path: Event Channel endpoints must use different Domains\n";
        $channel->{port_a} > 0 && $channel->{port_b} > 0
            or die "$path: Event Channel ports must be nonzero\n";
        $ids{$channel->{domain_a}} && $ids{$channel->{domain_b}}
            or die "$path: Event Channel refers to an unknown Domain\n";
        for my $endpoint (
            "$channel->{domain_a}:$channel->{port_a}",
            "$channel->{domain_b}:$channel->{port_b}",
        ) {
            !$endpoints{$endpoint}++
                or die "$path: Event Channel endpoint $endpoint is duplicated\n";
        }
    }

    $root{domains} = \@domains;
    $root{channels} = \@channels;
    return \%root;
}

sub role_id {
    my ($role) = @_;
    return 1 if $role eq 'system';
    return 2 if $role eq 'hardware';
    return 3 if $role eq 'application';
    die "unknown Domain role: $role\n";
}

sub parse_value {
    my ($raw, $path, $line_number) = @_;
    if ($raw =~ /^'(.*)'$/) {
        return $1;
    }
    if ($raw =~ /^"((?:[^"\\]|\\.)*)"$/) {
        my $value = $1;
        $value =~ s/\\([\\"nrt])/$1 eq 'n' ? "\n" : $1 eq 'r' ? "\r" : $1 eq 't' ? "\t" : $1/ge;
        return $value;
    }
    return 1 if $raw eq 'true';
    return 0 if $raw eq 'false';
    return hex($raw) if $raw =~ /^0x[0-9a-fA-F]+$/;
    return 0 + $raw if $raw =~ /^\d+$/;
    die "$path:$line_number: unsupported value\n";
}

1;
